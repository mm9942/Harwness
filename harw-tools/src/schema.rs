//! JSON-Schema-Teilmenge für Tool-Parameter.
//!
//! Bewusst minimal: genug, um Function-Tool-Parameter zu beschreiben, ohne die
//! volle JSON-Schema-Spezifikation zu modellieren.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Die zulässigen `type`-Werte eines JSON-Schemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsonSchemaType {
    String,
    Number,
    Integer,
    Boolean,
    Object,
    Array,
    Null,
}

/// Eine Teilmenge von JSON-Schema, ausreichend für Tool-Parameter.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<JsonSchemaType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(
        rename = "additionalProperties",
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_properties: Option<Box<AdditionalProperties>>,
    #[serde(rename = "anyOf", skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<JsonSchema>>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
}

impl JsonSchema {
    /// Normalisiert das Schema für den `strict`-Modus OpenAI-kompatibler Provider.
    ///
    /// # Description
    /// Im `strict`-Modus verlangt die Provider-Seite, dass `required` **jeden**
    /// Key aus `properties` enthält und dass `additionalProperties` auf jedem
    /// Objekt `false` ist. Optionalität wird dort nicht durch Weglassen aus
    /// `required` ausgedrückt, sondern durch einen nullable Typ. Diese Methode
    /// übersetzt die natürliche Schreibweise (`required` = echte Pflichtfelder)
    /// rekursiv in diese Form: jedes bisher nicht geforderte Feld wird zu
    /// `anyOf: [<original>, {"type": "null"}]` und wandert nach `required`.
    ///
    /// Die Umformung ist idempotent und lässt Schemata ohne `properties`
    /// (Skalare, Arrays ohne Objekt-Items) inhaltlich unverändert.
    ///
    /// # Returns
    /// Das strict-konforme Schema.
    #[must_use]
    pub fn into_strict(mut self) -> Self {
        if let Some(items) = self.items.take() {
            self.items = Some(Box::new(items.into_strict()));
        }
        if let Some(variants) = self.any_of.take() {
            self.any_of = Some(variants.into_iter().map(Self::into_strict).collect());
        }
        if let Some(Some(inner)) = self.additional_properties.take().map(|boxed| match *boxed {
            AdditionalProperties::Schema(schema) => Some(schema),
            AdditionalProperties::Bool(_) => None,
        }) {
            self.additional_properties = Some(Box::new(AdditionalProperties::Schema(Box::new(
                inner.into_strict(),
            ))));
        }

        let Some(properties) = self.properties.take() else {
            return self;
        };

        let required = self.required.take().unwrap_or_default();
        let mut normalized = BTreeMap::new();
        let mut keys = Vec::with_capacity(properties.len());
        for (name, schema) in properties {
            let schema = schema.into_strict();
            let schema = if required.iter().any(|entry| entry == &name) {
                schema
            } else {
                schema.into_nullable()
            };
            keys.push(name.clone());
            normalized.insert(name, schema);
        }

        self.properties = Some(normalized);
        self.required = Some(keys);
        if !matches!(
            self.additional_properties.as_deref(),
            Some(AdditionalProperties::Schema(_))
        ) {
            self.additional_properties = Some(Box::new(AdditionalProperties::Bool(false)));
        }
        self
    }

    // Macht ein Schema nullable, ohne die Beschreibung in den anyOf-Zweig zu ziehen.
    fn into_nullable(mut self) -> Self {
        let already_nullable = self.schema_type == Some(JsonSchemaType::Null)
            || self.any_of.as_ref().is_some_and(|variants| {
                variants
                    .iter()
                    .any(|variant| variant.schema_type == Some(JsonSchemaType::Null))
            });
        if already_nullable {
            return self;
        }

        let description = self.description.take();
        let null = JsonSchema {
            schema_type: Some(JsonSchemaType::Null),
            ..Default::default()
        };
        let variants = match self.any_of.take() {
            Some(mut variants) => {
                variants.push(null);
                variants
            }
            None => vec![self, null],
        };

        JsonSchema {
            description,
            any_of: Some(variants),
            ..Default::default()
        }
    }
}

/// `additionalProperties` ist entweder ein Bool oder ein verschachteltes Schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Bool(bool),
    Schema(Box<JsonSchema>),
}

#[cfg(test)]
mod tests {
    use super::{JsonSchema, JsonSchemaType};
    use crate::test_support::{TestError, TestResult, ctx};
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn default_is_omitted_when_absent() -> TestResult {
        let schema = JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            ..Default::default()
        };

        let serialized = serde_json::to_value(&schema).map_err(ctx("schema serializes"))?;

        assert_eq!(serialized, json!({"type": "string"}));
        Ok(())
    }

    #[test]
    fn default_round_trips_as_json_value() -> TestResult {
        let schema = JsonSchema {
            schema_type: Some(JsonSchemaType::Integer),
            default: Some(json!({"limit": 10, "enabled": true})),
            ..Default::default()
        };

        let serialized = serde_json::to_value(&schema).map_err(ctx("schema serializes"))?;
        assert_eq!(
            serialized,
            json!({"type": "integer", "default": {"limit": 10, "enabled": true}})
        );

        let deserialized: JsonSchema =
            serde_json::from_value(serialized).map_err(ctx("schema parses"))?;

        assert_eq!(deserialized, schema);
        Ok(())
    }

    // Baut ein Objekt-Schema mit einem Pflichtfeld (`path`) und einem
    // optionalen Feld (`max_bytes`), wie es der Bugreport beschreibt.
    fn object_with_required_and_optional_field() -> JsonSchema {
        let mut properties = BTreeMap::new();
        properties.insert(
            "path".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                ..Default::default()
            },
        );
        properties.insert(
            "max_bytes".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::Integer),
                description: Some("maximum number of bytes to read".to_owned()),
                ..Default::default()
            },
        );
        JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["path".to_owned()]),
            ..Default::default()
        }
    }

    #[test]
    fn test_into_strict_required_field_becomes_superset_of_properties() -> TestResult {
        let strict = object_with_required_and_optional_field().into_strict();

        let required = strict
            .required
            .ok_or(TestError::Missing("required present"))?;
        assert_eq!(required, vec!["max_bytes".to_owned(), "path".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_into_strict_optional_field_becomes_nullable_any_of() -> TestResult {
        let strict = object_with_required_and_optional_field().into_strict();

        let properties = strict
            .properties
            .ok_or(TestError::Missing("properties present"))?;
        let max_bytes = properties
            .get("max_bytes")
            .ok_or(TestError::Missing("max_bytes present"))?;
        let serialized = serde_json::to_value(max_bytes).map_err(ctx("schema serializes"))?;

        assert_eq!(
            serialized,
            json!({
                "description": "maximum number of bytes to read",
                "anyOf": [{"type": "integer"}, {"type": "null"}],
            })
        );
        Ok(())
    }

    #[test]
    fn test_into_strict_required_field_stays_unchanged() -> TestResult {
        let strict = object_with_required_and_optional_field().into_strict();

        let properties = strict
            .properties
            .ok_or(TestError::Missing("properties present"))?;
        let path = properties
            .get("path")
            .ok_or(TestError::Missing("path present"))?;
        let serialized = serde_json::to_value(path).map_err(ctx("schema serializes"))?;

        assert_eq!(serialized, json!({"type": "string"}));
        Ok(())
    }

    #[test]
    fn test_into_strict_sets_additional_properties_false_when_absent() {
        let strict = object_with_required_and_optional_field().into_strict();

        assert_eq!(
            strict.additional_properties.as_deref(),
            Some(&super::AdditionalProperties::Bool(false))
        );
    }

    #[test]
    fn test_into_strict_is_idempotent() {
        let once = object_with_required_and_optional_field().into_strict();
        let twice = once.clone().into_strict();

        assert_eq!(once, twice);
    }

    #[test]
    fn test_into_strict_without_properties_is_unchanged() {
        let schema = JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            description: Some("a plain string".to_owned()),
            ..Default::default()
        };

        let strict = schema.clone().into_strict();

        assert_eq!(strict, schema);
    }

    #[test]
    fn test_into_strict_recurses_into_nested_object_properties() -> TestResult {
        let mut inner_properties = BTreeMap::new();
        inner_properties.insert(
            "city".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                ..Default::default()
            },
        );
        inner_properties.insert(
            "zip".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                ..Default::default()
            },
        );
        let nested = JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(inner_properties),
            required: Some(vec!["city".to_owned()]),
            ..Default::default()
        };

        let mut outer_properties = BTreeMap::new();
        outer_properties.insert("address".to_owned(), nested);
        let outer = JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(outer_properties),
            required: Some(vec!["address".to_owned()]),
            ..Default::default()
        };

        let strict = outer.into_strict();
        let address = strict
            .properties
            .ok_or(TestError::Missing("outer properties present"))?
            .remove("address")
            .ok_or(TestError::Missing("address present"))?;

        assert_eq!(
            address.required,
            Some(vec!["city".to_owned(), "zip".to_owned()])
        );
        let zip = address
            .properties
            .ok_or(TestError::Missing("nested properties present"))?
            .remove("zip")
            .ok_or(TestError::Missing("zip present"))?;
        assert!(zip.any_of.is_some());
        Ok(())
    }

    #[test]
    fn test_into_strict_recurses_into_array_items() -> TestResult {
        let mut properties = BTreeMap::new();
        properties.insert(
            "name".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                ..Default::default()
            },
        );
        properties.insert(
            "score".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::Integer),
                ..Default::default()
            },
        );
        let item_schema = JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["name".to_owned()]),
            ..Default::default()
        };
        let array_schema = JsonSchema {
            schema_type: Some(JsonSchemaType::Array),
            items: Some(Box::new(item_schema)),
            ..Default::default()
        };

        let strict = array_schema.into_strict();
        let items = strict.items.ok_or(TestError::Missing("items present"))?;

        assert_eq!(
            items.required,
            Some(vec!["name".to_owned(), "score".to_owned()])
        );
        Ok(())
    }
}
