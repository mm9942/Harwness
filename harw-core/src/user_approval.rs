//! Vorab erteilte Freigaben der Nutzerin im Handoff (Runde 9, Teil E4).
//!
//! # Problem
//! Die Nutzerin sagte ausdrücklich „ohne weitere Freigabe", trotzdem bestand
//! die Spielleitung (`matrix-game-master`) auf ihrer Szenario-Freigabe
//! (Regel §3): im Auftrag des Kindes kam die Zustimmung nur als Prosa an,
//! die das Kind nicht als Freigabe werten durfte.
//!
//! # Lösung
//! `transfer_to_<rolle>` trägt ein optionales Feld
//! [`USER_APPROVED_FIELD`] (`["scenario", "plan"]`). Der Turn-Loop stellt
//! dem Auftrag des Kindes eine feste Zeile voran
//! ([`with_user_approval`]): „Die Nutzerin hat vorab freigegeben: scenario."
//! Die Rollenregel des Kindes nimmt genau diese Zeile als Freigabe an.
//!
//! # Sicherheit
//! Das Feld verleiht **keine** Rechte: keine Werkzeuge, keine Sandbox, kein
//! Budget. Es ist eine Aussage des Elternteils über die Nutzerin, die das
//! Kind wie jede andere Auftragszeile liest. Nur bekannte Werte
//! ([`USER_APPROVAL_VALUES`]) werden übernommen, alles andere verworfen.

use harw_tools::{JsonSchema, JsonSchemaType};

/// Name des optionalen Handoff-Felds.
pub const USER_APPROVED_FIELD: &str = "user_approved";

/// Die zulässigen Werte, in dieser Reihenfolge.
pub const USER_APPROVAL_VALUES: [&str; 2] = ["scenario", "plan"];

/// Anfang der Auftragszeile, die das Kind als Freigabe liest.
pub const USER_APPROVAL_PREAMBLE: &str = "Die Nutzerin hat vorab freigegeben:";

/// Das Schema des Felds (bewusst knapp: jede Beschreibung kostet Prompt).
#[must_use]
pub fn user_approved_schema() -> JsonSchema {
    JsonSchema {
        schema_type: Some(JsonSchemaType::Array),
        description: Some(
            "Optional: nur wenn die Nutzerin es ausdrücklich sagte — vorab erteilte Freigaben."
                .to_owned(),
        ),
        items: Some(Box::new(JsonSchema {
            schema_type: Some(JsonSchemaType::String),
            enum_values: Some(
                USER_APPROVAL_VALUES
                    .iter()
                    .map(|value| serde_json::Value::String((*value).to_owned()))
                    .collect(),
            ),
            ..JsonSchema::default()
        })),
        ..JsonSchema::default()
    }
}

/// Die bekannten, eindeutigen Freigaben aus den Handoff-Argumenten, in der
/// Reihenfolge von [`USER_APPROVAL_VALUES`]. Ein einzelner String wird wie
/// eine einelementige Liste gelesen; unbekannte Werte fallen weg.
#[must_use]
pub fn user_approvals(arguments: &serde_json::Value) -> Vec<&'static str> {
    let given: Vec<String> = match arguments.get(USER_APPROVED_FIELD) {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase())
            .collect(),
        Some(serde_json::Value::String(value)) => vec![value.trim().to_ascii_lowercase()],
        _ => Vec::new(),
    };
    USER_APPROVAL_VALUES
        .iter()
        .copied()
        .filter(|known| given.iter().any(|value| value == known))
        .collect()
}

/// Die Auftragszeile zu den Freigaben, falls es welche gibt.
#[must_use]
pub fn user_approval_preamble(arguments: &serde_json::Value) -> Option<String> {
    let approvals = user_approvals(arguments);
    (!approvals.is_empty()).then(|| format!("{USER_APPROVAL_PREAMBLE} {}.", approvals.join(", ")))
}

/// Stellt dem Auftrag die Freigabe-Zeile voran (unverändert ohne Freigaben).
#[must_use]
pub fn with_user_approval(
    instructions: Option<String>,
    arguments: &serde_json::Value,
) -> Option<String> {
    match (user_approval_preamble(arguments), instructions) {
        (Some(preamble), Some(task)) => Some(format!("{preamble}\n\n{task}")),
        (Some(preamble), None) => Some(preamble),
        (None, instructions) => instructions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_known_values_are_taken_in_a_fixed_order() {
        assert_eq!(
            user_approvals(&json!({ "user_approved": ["plan", "Scenario", "alles", 3] })),
            vec!["scenario", "plan"]
        );
        assert_eq!(
            user_approvals(&json!({ "user_approved": "scenario" })),
            vec!["scenario"]
        );
        assert!(user_approvals(&json!({ "user_approved": ["tools"] })).is_empty());
        assert!(user_approvals(&json!({ "task": "x" })).is_empty());
    }

    #[test]
    fn the_preamble_leads_the_task_and_is_absent_without_approvals() {
        let args = json!({ "task": "Spiele das Szenario", "user_approved": ["scenario"] });
        assert_eq!(
            with_user_approval(Some("Spiele das Szenario".to_owned()), &args).as_deref(),
            Some("Die Nutzerin hat vorab freigegeben: scenario.\n\nSpiele das Szenario")
        );
        let plain = json!({ "task": "Spiele" });
        assert_eq!(
            with_user_approval(Some("Spiele".to_owned()), &plain).as_deref(),
            Some("Spiele")
        );
        assert_eq!(with_user_approval(None, &plain), None);
    }

    #[test]
    fn the_schema_is_a_short_enum_array() {
        let schema = user_approved_schema();
        assert_eq!(schema.schema_type, Some(JsonSchemaType::Array));
        assert!(
            schema
                .description
                .as_deref()
                .is_some_and(|text| text.len() < 100)
        );
        let items = schema
            .items
            .as_deref()
            .map(|items| items.enum_values.clone());
        assert_eq!(items, Some(Some(vec![json!("scenario"), json!("plan")])));
    }
}
