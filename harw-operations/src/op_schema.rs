//! JSON-Schema-Vokabular für Operations-Argumente auf der Modell-Tool-Fläche.
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt den **Vertrag**, gegen den `#[derive(OpArgs)]` in
//! `harw-macros` generiert: den Trait [`OpArgsSchema`] und die Bauhelfer, die
//! aus Feld-Informationen ein geschlossenes JSON-Objekt-Schema
//! (`additionalProperties: false`) zusammensetzen.
//!
//! Es besitzt **nicht** die Typ-Inferenz (die lebt im Makro, `harw-macros/src/schema.rs`)
//! und **nicht** die Zuordnung Tool-Name → Schema (die lebt in
//! [`crate::adapter::model_tool`]).
//!
//! # Motivation
//! [`crate::adapter::model_tool::model_tool_schema_for`] pflegte bisher einen
//! hartcodierten `match` über Tool-Namen. Jede neue Operation mit
//! `Surface::ModelTool` musste dort einen Arm ergänzen — wurde er vergessen,
//! bekam das Modell ein offenes Objekt-Schema ohne jede Feldinformation.
//! [`OpArgsSchema`] verlagert diese Information an den Ort, an dem sie ohnehin
//! steht: an den Argument-Typ der Operation.
//!
//! # Schlüsseltypen
//! - [`OpArgsSchema`] — Trait, den das Derive-Makro implementiert.
//!
//! # Nebenläufigkeit
//! Alle Funktionen dieses Moduls sind rein (keine Sperren, keine Threads, kein
//! gemeinsamer veränderlicher Zustand) und damit uneingeschränkt aus mehreren
//! Threads aufrufbar. [`OpArgsSchema::json_schema`] ist eine assoziierte
//! Funktion ohne `self`; der Trait ist deshalb bewusst **nicht** objekt-sicher
//! und wird als `fn() -> JsonSchema`-Zeiger weitergereicht.
//!
//! # Fehlertypen
//! Keine. Die Schema-Erzeugung ist total — ungültige Deklarationen werden
//! bereits zur Makro-Expansionszeit als Compile-Fehler abgewiesen.
//!
//! # Beispiel
//! ```rust
//! use harw_operations::op_schema::{OpArgsSchema, object_schema, string_schema};
//! use harw_tools::JsonSchema;
//!
//! struct StopArgs;
//!
//! impl OpArgsSchema for StopArgs {
//!     fn json_schema() -> JsonSchema {
//!         object_schema(
//!             vec![("job_id", string_schema("Kennung des zu stoppenden Jobs."))],
//!             &["job_id"],
//!         )
//!     }
//! }
//!
//! let schema = StopArgs::json_schema();
//! assert_eq!(schema.required.as_deref(), Some(&["job_id".to_owned()][..]));
//! ```

use std::collections::BTreeMap;

use harw_tools::{AdditionalProperties, JsonSchema, JsonSchemaType};

/// Liefert das JSON-Schema der Argumente einer Operation für die Model-Tool-Fläche.
///
/// # Beschreibung
/// Implementierungen werden normalerweise von `#[derive(OpArgs)]` erzeugt und
/// nicht von Hand geschrieben. Das erzeugte Schema ist immer ein
/// **geschlossenes** Objekt (`additionalProperties: false`) mit `properties`
/// und — sofern es Pflichtfelder gibt — `required`, sodass das Modell keine
/// erfundenen Argumentnamen einsetzen kann.
///
/// Der Trait hat bewusst keine `self`-Methode: das Schema ist eine reine
/// Typ-Eigenschaft und muss ohne Instanz abrufbar sein (z. B. als
/// `fn() -> JsonSchema` in einem künftigen `OperationMeta::args_schema`-Feld).
/// Damit ist der Trait nicht objekt-sicher, was hier gewollt ist.
///
/// # Nebenläufigkeit
/// Implementierungen müssen rein sein (kein I/O, keine Sperren) und sind aus
/// beliebig vielen Threads gleichzeitig aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::{OpArgsSchema, object_schema, bool_schema};
///
/// struct DiffArgs;
///
/// impl OpArgsSchema for DiffArgs {
///     fn json_schema() -> harw_tools::JsonSchema {
///         object_schema(vec![("stat_only", bool_schema("Nur Statistik ausgeben."))], &[])
///     }
/// }
///
/// assert!(DiffArgs::json_schema().required.is_none());
/// ```
pub trait OpArgsSchema {
    /// Geschlossenes Schema (`additionalProperties: false`) mit `properties` und `required`.
    ///
    /// # Rückgabe
    /// [`JsonSchema`] vom Typ `object`. `properties` ist immer gesetzt (ggf. leer),
    /// `required` nur, wenn mindestens ein Pflichtfeld existiert.
    fn json_schema() -> JsonSchema;
}

/// Baut ein geschlossenes Objekt-Schema aus Properties und Pflichtfeldern.
///
/// # Beschreibung
/// Setzt `type: "object"` und `additionalProperties: false`. `properties` wird
/// **immer** gesetzt (auch bei leerer Property-Liste), damit ein argumentloses
/// Tool ein explizit leeres, geschlossenes Schema erhält statt eines offenen
/// Objekts. `required` bleibt `None`, wenn `required` leer ist — ein leeres
/// `required: []` wäre für das Modell nur Rauschen.
///
/// # Argumente
/// - `props` (`Vec<(&'static str, JsonSchema)>`): Property-Name → Schema, in
///   Deklarationsreihenfolge. Bei doppeltem Namen gewinnt der **erste** Eintrag;
///   das entspricht der Vereinigungsregel des `OpArgs`-Derives für
///   Subcommand-Enums (die erste Variante bestimmt die Beschreibung).
/// - `required` (`&[&'static str]`): Namen der Pflicht-Properties. Duplikate
///   werden entfernt, die Reihenfolge bleibt erhalten. Der Aufrufer ist dafür
///   verantwortlich, dass jeder Name auch in `props` vorkommt.
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `object` ohne Wurzel-`description`.
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::{object_schema, string_schema};
///
/// let schema = object_schema(vec![("path", string_schema("Pfad."))], &["path"]);
/// assert_eq!(schema.properties.map(|p| p.len()), Some(1));
/// ```
#[must_use]
pub fn object_schema(
    props: Vec<(&'static str, JsonSchema)>,
    required: &[&'static str],
) -> JsonSchema {
    described_object_schema("", props, required)
}

/// Wie [`object_schema`], zusätzlich mit einer `description` am Wurzelknoten.
///
/// # Beschreibung
/// Für Subcommand-Enums ist die Wurzel-`description` die eigentliche Anleitung
/// für das Modell: sie listet alle Subcommands mit ihren Feldern auf, weil ein
/// flaches Objekt-Schema diese Zuordnung nicht ausdrücken kann.
///
/// # Argumente
/// - `description` (`&str`): Wurzelbeschreibung. Ein leerer oder nur aus
///   Leerraum bestehender Wert erzeugt **kein** `description`-Feld.
/// - `props` (`Vec<(&'static str, JsonSchema)>`): siehe [`object_schema`].
/// - `required` (`&[&'static str]`): siehe [`object_schema`].
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `object`.
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::{described_object_schema, string_schema};
///
/// let schema = described_object_schema(
///     "Subcommands: `list`, `show`.",
///     vec![("id", string_schema("Kennung."))],
///     &[],
/// );
/// assert!(schema.description.is_some());
/// ```
#[must_use]
pub fn described_object_schema(
    description: &str,
    props: Vec<(&'static str, JsonSchema)>,
    required: &[&'static str],
) -> JsonSchema {
    let mut properties: BTreeMap<String, JsonSchema> = BTreeMap::new();
    for (name, schema) in props {
        // Erster Eintrag gewinnt — Vereinigungsregel für Subcommand-Enums.
        properties.entry(name.to_owned()).or_insert(schema);
    }

    let mut required_names: Vec<String> = Vec::with_capacity(required.len());
    for name in required {
        let name = (*name).to_owned();
        if !required_names.contains(&name) {
            required_names.push(name);
        }
    }

    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        description: optional_description(description),
        properties: Some(properties),
        required: if required_names.is_empty() {
            None
        } else {
            Some(required_names)
        },
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

/// Baut ein `type: "string"`-Schema mit optionaler Beschreibung.
///
/// # Argumente
/// - `description` (`&str`): Feldbeschreibung; leer oder nur Leerraum ⇒ kein
///   `description`-Feld im Schema.
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `string`.
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::string_schema;
///
/// assert!(string_schema("").description.is_none());
/// ```
#[must_use]
pub fn string_schema(description: &str) -> JsonSchema {
    scalar_schema(JsonSchemaType::String, description)
}

/// Baut ein `type: "boolean"`-Schema mit optionaler Beschreibung.
///
/// # Argumente
/// - `description` (`&str`): siehe [`string_schema`].
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `boolean`.
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::bool_schema;
/// use harw_tools::JsonSchemaType;
///
/// assert_eq!(bool_schema("Nur zählen.").schema_type, Some(JsonSchemaType::Boolean));
/// ```
#[must_use]
pub fn bool_schema(description: &str) -> JsonSchema {
    scalar_schema(JsonSchemaType::Boolean, description)
}

/// Baut ein `type: "integer"`-Schema mit optionaler Beschreibung.
///
/// # Argumente
/// - `description` (`&str`): siehe [`string_schema`].
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `integer`.
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::integer_schema;
/// use harw_tools::JsonSchemaType;
///
/// assert_eq!(integer_schema("Maximale Treffer.").schema_type, Some(JsonSchemaType::Integer));
/// ```
#[must_use]
pub fn integer_schema(description: &str) -> JsonSchema {
    scalar_schema(JsonSchemaType::Integer, description)
}

/// Baut ein `type: "number"`-Schema mit optionaler Beschreibung.
///
/// # Beschreibung
/// Gegenstück zu [`integer_schema`] für `f32`/`f64`-Felder; die Typ-Inferenz des
/// `OpArgs`-Derives bildet Fließkommatypen auf `number` ab.
///
/// # Argumente
/// - `description` (`&str`): siehe [`string_schema`].
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `number`.
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::number_schema;
/// use harw_tools::JsonSchemaType;
///
/// assert_eq!(number_schema("Schwellwert.").schema_type, Some(JsonSchemaType::Number));
/// ```
#[must_use]
pub fn number_schema(description: &str) -> JsonSchema {
    scalar_schema(JsonSchemaType::Number, description)
}

/// Baut ein `type: "array"`-Schema mit dem übergebenen Element-Schema.
///
/// # Argumente
/// - `items` (`JsonSchema`): Schema eines einzelnen Elements. Wird per `Box`
///   in das Array-Schema übernommen (Eigentumsübergabe).
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `array` mit gesetztem `items`.
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::{array_schema, string_schema};
///
/// let schema = array_schema(string_schema(""));
/// assert!(schema.items.is_some());
/// ```
#[must_use]
pub fn array_schema(items: JsonSchema) -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Array),
        items: Some(Box::new(items)),
        ..Default::default()
    }
}

/// Baut ein `type: "string"`-Schema mit fester Wertemenge (`enum`).
///
/// # Beschreibung
/// Wird vom `OpArgs`-Derive für die Pflicht-Property `action` eines
/// Subcommand-Enums verwendet: die Werte sind die kebab-case-Namen der
/// Enum-Varianten. Duplikate werden entfernt, die Reihenfolge bleibt erhalten.
///
/// # Argumente
/// - `description` (`&str`): siehe [`string_schema`].
/// - `variants` (`&[&str]`): erlaubte Zeichenketten in Deklarationsreihenfolge.
///
/// # Rückgabe
/// [`JsonSchema`] vom Typ `string` mit gesetztem `enum`. Bei leerer
/// Variantenliste bleibt `enum` `None` (ein leeres `enum` wäre unerfüllbar).
///
/// # Nebenläufigkeit
/// Rein; aus beliebig vielen Threads aufrufbar.
///
/// # Beispiel
/// ```rust
/// use harw_operations::op_schema::enum_string_schema;
///
/// let schema = enum_string_schema("Subcommand.", &["list", "show"]);
/// assert_eq!(schema.enum_values.map(|v| v.len()), Some(2));
/// ```
#[must_use]
pub fn enum_string_schema(description: &str, variants: &[&str]) -> JsonSchema {
    let mut values: Vec<serde_json::Value> = Vec::with_capacity(variants.len());
    for variant in variants {
        let value = serde_json::Value::String((*variant).to_owned());
        if !values.contains(&value) {
            values.push(value);
        }
    }

    JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: optional_description(description),
        enum_values: if values.is_empty() {
            None
        } else {
            Some(values)
        },
        ..Default::default()
    }
}

// Gemeinsamer Bauplan für alle skalaren Schemata.
fn scalar_schema(schema_type: JsonSchemaType, description: &str) -> JsonSchema {
    JsonSchema {
        schema_type: Some(schema_type),
        description: optional_description(description),
        ..Default::default()
    }
}

// Leere bzw. reine Leerraum-Beschreibungen erzeugen kein `description`-Feld.
fn optional_description(description: &str) -> Option<String> {
    if description.trim().is_empty() {
        None
    } else {
        Some(description.to_owned())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        OpArgsSchema, array_schema, bool_schema, described_object_schema, enum_string_schema,
        integer_schema, number_schema, object_schema, string_schema,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_tools::{AdditionalProperties, JsonSchema, JsonSchemaType};

    #[test]
    fn test_object_schema_is_closed_object() {
        let schema = object_schema(vec![("path", string_schema("Pfad."))], &["path"]);

        assert_eq!(schema.schema_type, Some(JsonSchemaType::Object));
        assert_eq!(
            schema.additional_properties,
            Some(Box::new(AdditionalProperties::Bool(false))),
            "Objekt-Schemata müssen geschlossen sein"
        );
    }

    #[test]
    fn test_object_schema_empty_properties_still_present() -> TestResult {
        let schema = object_schema(Vec::new(), &[]);

        let properties = schema.properties.ok_or(TestError::Missing(
            "auch ein argumentloses Tool bekommt eine leere Property-Menge",
        ))?;
        assert!(properties.is_empty());
        Ok(())
    }

    #[test]
    fn test_object_schema_required_list_preserves_order() {
        let schema = object_schema(
            vec![
                ("goal", string_schema("Ziel.")),
                ("depth", integer_schema("Tiefe.")),
            ],
            &["goal", "depth"],
        );

        assert_eq!(
            schema.required,
            Some(vec!["goal".to_owned(), "depth".to_owned()])
        );
    }

    #[test]
    fn test_object_schema_empty_required_is_none() {
        let schema = object_schema(vec![("goal", string_schema("Ziel."))], &[]);

        assert_eq!(
            schema.required, None,
            "ein leeres `required` wäre für das Modell nur Rauschen"
        );
    }

    #[test]
    fn test_object_schema_duplicate_required_is_deduplicated() {
        let schema = object_schema(vec![("goal", string_schema("Ziel."))], &["goal", "goal"]);

        assert_eq!(schema.required, Some(vec!["goal".to_owned()]));
    }

    #[test]
    fn test_object_schema_duplicate_property_keeps_first_description() -> TestResult {
        let schema = object_schema(
            vec![
                ("goal", string_schema("Erste Beschreibung.")),
                ("goal", string_schema("Zweite Beschreibung.")),
            ],
            &[],
        );

        let properties = schema
            .properties
            .ok_or(TestError::Missing("properties gesetzt"))?;
        assert_eq!(properties.len(), 1);
        assert_eq!(
            properties["goal"].description.as_deref(),
            Some("Erste Beschreibung."),
            "bei doppeltem Feldnamen gewinnt der erste Eintrag"
        );
        Ok(())
    }

    #[test]
    fn test_described_object_schema_sets_root_description() {
        let schema = described_object_schema("Subcommands: `list`.", Vec::new(), &[]);

        assert_eq!(schema.description.as_deref(), Some("Subcommands: `list`."));
    }

    #[test]
    fn test_described_object_schema_blank_description_is_omitted() {
        let schema = described_object_schema("   ", Vec::new(), &[]);

        assert_eq!(schema.description, None);
    }

    #[test]
    fn test_string_schema_sets_type_and_description() {
        let schema = string_schema("Pfad relativ zum Workspace.");

        assert_eq!(schema.schema_type, Some(JsonSchemaType::String));
        assert_eq!(
            schema.description.as_deref(),
            Some("Pfad relativ zum Workspace.")
        );
    }

    #[test]
    fn test_string_schema_empty_description_is_omitted() {
        assert_eq!(string_schema("").description, None);
    }

    #[test]
    fn test_bool_schema_sets_boolean_type() {
        assert_eq!(
            bool_schema("Nur Statistik.").schema_type,
            Some(JsonSchemaType::Boolean)
        );
    }

    #[test]
    fn test_integer_schema_sets_integer_type() {
        assert_eq!(
            integer_schema("Maximale Treffer.").schema_type,
            Some(JsonSchemaType::Integer)
        );
    }

    #[test]
    fn test_number_schema_sets_number_type() {
        assert_eq!(
            number_schema("Schwellwert.").schema_type,
            Some(JsonSchemaType::Number)
        );
    }

    #[test]
    fn test_array_schema_wraps_item_schema() -> TestResult {
        let schema = array_schema(string_schema("Ein Pfad."));

        assert_eq!(schema.schema_type, Some(JsonSchemaType::Array));
        let items = schema.items.ok_or(TestError::Missing("items gesetzt"))?;
        assert_eq!(items.schema_type, Some(JsonSchemaType::String));
        Ok(())
    }

    #[test]
    fn test_enum_string_schema_lists_variants_in_order() {
        let schema = enum_string_schema("Subcommand.", &["plan", "goal", "explore"]);

        assert_eq!(schema.schema_type, Some(JsonSchemaType::String));
        assert_eq!(
            schema.enum_values,
            Some(vec![
                serde_json::Value::String("plan".to_owned()),
                serde_json::Value::String("goal".to_owned()),
                serde_json::Value::String("explore".to_owned()),
            ])
        );
    }

    #[test]
    fn test_enum_string_schema_deduplicates_variants() {
        let schema = enum_string_schema("", &["list", "list"]);

        assert_eq!(schema.enum_values.map(|values| values.len()), Some(1));
    }

    #[test]
    fn test_enum_string_schema_without_variants_omits_enum() {
        assert_eq!(
            enum_string_schema("", &[]).enum_values,
            None,
            "ein leeres `enum` wäre unerfüllbar"
        );
    }

    #[test]
    fn test_op_args_schema_impl_serializes_to_closed_json() -> TestResult {
        struct StopArgs;

        impl OpArgsSchema for StopArgs {
            fn json_schema() -> JsonSchema {
                object_schema(
                    vec![("job_id", string_schema("Kennung des Jobs."))],
                    &["job_id"],
                )
            }
        }

        let serialized =
            serde_json::to_value(StopArgs::json_schema()).map_err(ctx("Schema serialisiert"))?;

        assert_eq!(
            serialized,
            serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "Kennung des Jobs." }
                },
                "required": ["job_id"],
                "additionalProperties": false
            })
        );
        Ok(())
    }
}
