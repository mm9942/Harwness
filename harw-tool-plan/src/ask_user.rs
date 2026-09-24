//! Argumente und Prüfung von `ask_user` (Runde 5, Teil F, Punkt 4).
//!
//! # Beschreibung
//! 1–4 Fragen mit je 2–4 Optionen; die TUI ergänzt immer die Freitext-Option
//! „Andere“. Jede Frage ist Einzel- oder Mehrfachauswahl. Alle Texte sind
//! Modelltext und werden deshalb streng geprüft: keine Steuer- und keine
//! unsichtbaren Formatzeichen (keine Täuschung im Auswahlfenster), feste
//! Höchstlängen, eindeutige Optionslabels je Frage.
//!
//! # Nebenläufigkeit
//! Reine Werte und Funktionen.

use std::collections::BTreeMap;

use harw_tools::schema::{AdditionalProperties, JsonSchema, JsonSchemaType};
use serde::{Deserialize, Serialize};

use crate::prompt::AskUserAnswer;

/// Mindest- und Höchstzahl der Fragen.
pub const QUESTIONS_RANGE: (usize, usize) = (1, 4);
/// Mindest- und Höchstzahl der Optionen je Frage (ohne „Andere“).
pub const OPTIONS_RANGE: (usize, usize) = (2, 4);
/// Höchstlänge eines Fragetexts in Zeichen.
pub const MAX_QUESTION_CHARS: usize = 500;
/// Höchstlänge einer Kurzüberschrift in Zeichen.
pub const MAX_HEADER_CHARS: usize = 24;
/// Höchstlänge eines Optionslabels in Zeichen.
pub const MAX_LABEL_CHARS: usize = 80;
/// Höchstlänge einer Optionsbeschreibung in Zeichen.
pub const MAX_DESCRIPTION_CHARS: usize = 300;
/// Höchstlänge des Freitexts „Andere“ in Zeichen (Durchsetzung in der TUI).
pub const MAX_OTHER_CHARS: usize = 1000;

/// Eine Antwortoption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskOption {
    /// Kurzes Label (wird in der Antwort zurückgegeben).
    pub label: String,
    /// Optionale Erläuterung.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Eine Frage an die Nutzerin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskQuestion {
    /// Der Fragetext.
    pub question: String,
    /// Optionale Kurzüberschrift (Reiter im Auswahlfenster).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// 2–4 Optionen.
    pub options: Vec<AskOption>,
    /// `true` erlaubt mehrere Optionen.
    #[serde(default)]
    pub multi_select: bool,
}

/// Die Argumente von `ask_user`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskUserArgs {
    /// 1–4 Fragen.
    pub questions: Vec<AskQuestion>,
}

/// Prüft die Argumente vollständig.
///
/// # Errors
/// Eine deutsche Meldung für das Modell, die den ersten Verstoß nennt.
pub fn validate(args: &AskUserArgs) -> Result<(), String> {
    let (min_q, max_q) = QUESTIONS_RANGE;
    if !(min_q..=max_q).contains(&args.questions.len()) {
        return Err(format!(
            "ask_user braucht {min_q}–{max_q} Fragen, bekam {}",
            args.questions.len()
        ));
    }
    for (index, question) in args.questions.iter().enumerate() {
        let number = index + 1;
        check_text(&question.question, MAX_QUESTION_CHARS, false)
            .map_err(|why| format!("Frage {number}: Fragetext {why}"))?;
        if let Some(header) = &question.header {
            check_text(header, MAX_HEADER_CHARS, true)
                .map_err(|why| format!("Frage {number}: header {why}"))?;
        }
        let (min_o, max_o) = OPTIONS_RANGE;
        if !(min_o..=max_o).contains(&question.options.len()) {
            return Err(format!(
                "Frage {number}: {min_o}–{max_o} Optionen nötig (\"Andere\" ergänzt die \
                 Oberfläche selbst), bekam {}",
                question.options.len()
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for (option_index, option) in question.options.iter().enumerate() {
            let option_number = option_index + 1;
            check_text(&option.label, MAX_LABEL_CHARS, true)
                .map_err(|why| format!("Frage {number}, Option {option_number}: label {why}"))?;
            if let Some(description) = &option.description {
                check_text(description, MAX_DESCRIPTION_CHARS, false).map_err(|why| {
                    format!("Frage {number}, Option {option_number}: description {why}")
                })?;
            }
            let label = option.label.trim();
            if seen.iter().any(|other| other.eq_ignore_ascii_case(label)) {
                return Err(format!("Frage {number}: Option „{label}“ ist doppelt"));
            }
            seen.push(label);
        }
    }
    Ok(())
}

/// Prüft einen Anzeigetext. `single_line` verbietet zusätzlich Zeilenumbrüche.
fn check_text(text: &str, max_chars: usize, single_line: bool) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("ist leer".to_owned());
    }
    if text.chars().count() > max_chars {
        return Err(format!("ist länger als {max_chars} Zeichen"));
    }
    if let Some(bad) = text.chars().find(|c| is_forbidden_char(*c, single_line)) {
        return Err(format!(
            "enthält ein unzulässiges Steuer- oder Formatzeichen (U+{:04X})",
            u32::from(bad)
        ));
    }
    Ok(())
}

/// Steuerzeichen (außer `\n` in mehrzeiligen Texten) und unsichtbare
/// Format-/Richtungszeichen.
pub(crate) fn is_forbidden_char(c: char, single_line: bool) -> bool {
    if c == '\n' {
        return single_line;
    }
    c.is_control()
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{00AD}'
        )
}

/// Formt die Antwort der Nutzerin zum Werkzeugergebnis.
#[must_use]
pub fn answer_json(answer: &AskUserAnswer) -> serde_json::Value {
    serde_json::json!({
        "answers": answer.answers,
        "hint": "Antworten der Nutzerin. Richte den weiteren Plan danach aus; \
                 frage nicht erneut dasselbe.",
    })
}

/// Das Parameterschema von `ask_user`.
#[must_use]
pub fn parameter_schema() -> JsonSchema {
    let string = |description: &str| JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: Some(description.to_owned()),
        ..Default::default()
    };
    let mut option_props = BTreeMap::new();
    option_props.insert(
        "label".to_owned(),
        string("Short option label (max 80 chars), returned as the answer."),
    );
    option_props.insert(
        "description".to_owned(),
        string("Optional one-line explanation of the option and its trade-off."),
    );
    let option = JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(option_props),
        required: Some(vec!["label".to_owned()]),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    };
    let mut question_props = BTreeMap::new();
    question_props.insert(
        "question".to_owned(),
        string("The complete question, ending with a question mark."),
    );
    question_props.insert(
        "header".to_owned(),
        string("Optional very short tab label (max 24 chars), e.g. \"Auth\"."),
    );
    question_props.insert(
        "options".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Array),
            description: Some(
                "2-4 distinct options. Do NOT add an \"Other\" option; the UI always offers \
                 free text."
                    .to_owned(),
            ),
            items: Some(Box::new(option)),
            ..Default::default()
        },
    );
    question_props.insert(
        "multi_select".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Boolean),
            description: Some("true lets the user pick several options.".to_owned()),
            ..Default::default()
        },
    );
    let question = JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(question_props),
        required: Some(vec!["question".to_owned(), "options".to_owned()]),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    };
    let mut properties = BTreeMap::new();
    properties.insert(
        "questions".to_owned(),
        JsonSchema {
            schema_type: Some(JsonSchemaType::Array),
            description: Some("1-4 questions shown together in one selection window.".to_owned()),
            items: Some(Box::new(question)),
            ..Default::default()
        },
    );
    JsonSchema {
        schema_type: Some(JsonSchemaType::Object),
        properties: Some(properties),
        required: Some(vec!["questions".to_owned()]),
        additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::QuestionAnswer;
    use crate::test_support::{TestResult, ctx};

    fn args(value: serde_json::Value) -> TestResult<AskUserArgs> {
        serde_json::from_value(value).map_err(ctx("args"))
    }

    fn question(options: usize) -> serde_json::Value {
        let options: Vec<serde_json::Value> = (0..options)
            .map(|i| serde_json::json!({ "label": format!("Option {i}") }))
            .collect();
        serde_json::json!({ "question": "Welche Variante?", "options": options })
    }

    #[test]
    fn accepts_one_to_four_questions_with_two_to_four_options() -> TestResult {
        for count in 1..=4 {
            let questions: Vec<_> = (0..count).map(|_| question(3)).collect();
            assert!(validate(&args(serde_json::json!({ "questions": questions }))?).is_ok());
        }
        for options in 2..=4 {
            assert!(
                validate(&args(
                    serde_json::json!({ "questions": [question(options)] })
                )?)
                .is_ok()
            );
        }
        Ok(())
    }

    #[test]
    fn rejects_counts_outside_the_ranges() -> TestResult {
        assert!(validate(&args(serde_json::json!({ "questions": [] }))?).is_err());
        let five: Vec<_> = (0..5).map(|_| question(2)).collect();
        assert!(validate(&args(serde_json::json!({ "questions": five }))?).is_err());
        assert!(validate(&args(serde_json::json!({ "questions": [question(1)] }))?).is_err());
        assert!(validate(&args(serde_json::json!({ "questions": [question(5)] }))?).is_err());
        Ok(())
    }

    #[test]
    fn rejects_control_and_bidi_characters_and_duplicate_labels() -> TestResult {
        let bidi = serde_json::json!({ "questions": [{
            "question": "Frage?",
            "options": [{ "label": "a\u{202E}b" }, { "label": "c" }]
        }]});
        assert!(validate(&args(bidi)?).is_err());
        let newline_label = serde_json::json!({ "questions": [{
            "question": "Frage?",
            "options": [{ "label": "a\nb" }, { "label": "c" }]
        }]});
        assert!(validate(&args(newline_label)?).is_err());
        let dup = serde_json::json!({ "questions": [{
            "question": "Frage?",
            "options": [{ "label": "Ja" }, { "label": "ja" }]
        }]});
        assert!(validate(&args(dup)?).is_err());
        let unknown = serde_json::json!({ "questions": [question(2)], "extra": 1 });
        assert!(serde_json::from_value::<AskUserArgs>(unknown).is_err());
        Ok(())
    }

    #[test]
    fn answer_json_carries_every_answer() {
        let answer = AskUserAnswer {
            answers: vec![QuestionAnswer {
                question: "Welche Variante?".to_owned(),
                selected: vec!["A".to_owned()],
                other: Some("lieber C".to_owned()),
            }],
        };
        let json = answer_json(&answer);
        assert_eq!(json["answers"][0]["selected"][0], "A");
        assert_eq!(json["answers"][0]["other"], "lieber C");
    }
}
