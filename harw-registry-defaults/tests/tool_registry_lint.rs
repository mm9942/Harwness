//! Registry-Lint (PL-97 B2, PL-88): jedes registrierte Werkzeug erklärt sich
//! und seine Argumente dem Modell.
//!
//! # Regel (eine Stelle: [`lint_tool_spec`])
//! - Die Beschreibung hat mindestens [`MIN_DESCRIPTION_CHARS`] Zeichen
//!   (Unicode-Zeichen, Rand-Leerraum nicht mitgezählt). Ihr erster Satz ist
//!   die Kurzbeschreibung in `tool.index` und `harw tool list`; ein
//!   Zehn-Zeichen-Text sagt dem Modell nicht, wann es das Werkzeug nimmt.
//! - Jedes Argumentfeld (`properties` der obersten Ebene) hat eine nicht
//!   leere Beschreibung.
//!
//! # Geltungsbereich
//! Alle Werkzeuge, die irgendein `RegistryProfile` registriert, mit ihren
//! echten Spezifikationen (`profile::known_tool_specs`, dieselben Provider wie
//! `profile_tool_providers`). Die Operations-Werkzeuge (`dod.*`,
//! `tool.index`, …) prüft `harw-ops` (dieses Crate sieht sie nicht).
//!
//! # Ausnahmen
//! Werkzeuge, die die Regel heute verletzen, stehen in [`ALLOWLIST`] — je
//! Eintrag mit Grund. Die Regel selbst wird nicht abgeschwächt, und ein
//! Eintrag, dessen Werkzeug die Regel inzwischen erfüllt oder das niemand
//! mehr registriert, lässt den Test fehlschlagen (veraltete Ausnahme).

use std::collections::BTreeMap;

use harw_registry_defaults::profile::known_tool_specs;
use harw_tools::{FunctionToolSpec, JsonSchema, JsonSchemaType, ToolName, ToolSpec};

mod common;
use common::{TestError, TestResult};

/// Mindestlänge einer Werkzeugbeschreibung in Zeichen.
const MIN_DESCRIPTION_CHARS: usize = 60;

/// Werkzeuge, die die Regel heute verletzen: `(Werkzeug, Grund)`.
const ALLOWLIST: &[(&str, &str)] = &[(
    "fs.glob",
    "49 Zeichen (\"Findet Dateien im Workspace über ein Glob-Muster.\"); die \
     Beschreibung liegt in harw-tool-fs/src/glob.rs und ist im Golden-Test \
     harw-tool-fs/tests/golden/fs_tool_specs.json festgeschrieben — beides \
     außerhalb von PL-97 B2, Nachzug dort",
)];

/// Ein Verstoß gegen die Regel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LintIssue {
    /// Die Beschreibung ist kürzer als [`MIN_DESCRIPTION_CHARS`].
    ShortDescription {
        /// Gezählte Zeichen.
        chars: usize,
    },
    /// Ein Argumentfeld ohne Beschreibung.
    UndocumentedField(String),
}

/// Prüft eine Spezifikation gegen die Regel (siehe Moduldoku).
fn lint_tool_spec(spec: &ToolSpec) -> Vec<LintIssue> {
    let ToolSpec::Function(function) = spec;
    let mut issues = Vec::new();
    let chars = function.description.trim().chars().count();
    if chars < MIN_DESCRIPTION_CHARS {
        issues.push(LintIssue::ShortDescription { chars });
    }
    for (field, schema) in function.parameters.properties.iter().flatten() {
        let documented = schema
            .description
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty());
        if !documented {
            issues.push(LintIssue::UndocumentedField(field.clone()));
        }
    }
    issues
}

/// Ein Fixture-Werkzeug mit Beschreibung und einem Feld.
fn fixture(description: &str, field_description: Option<&str>) -> ToolSpec {
    let field = JsonSchema {
        schema_type: Some(JsonSchemaType::String),
        description: field_description.map(str::to_owned),
        ..JsonSchema::default()
    };
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new("fixture.tool"),
        description: description.to_owned(),
        parameters: JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(BTreeMap::from([("path".to_owned(), field)])),
            required: Some(vec!["path".to_owned()]),
            ..JsonSchema::default()
        },
        strict: false,
    })
}

#[test]
fn every_registered_tool_documents_itself_and_its_arguments() -> TestResult {
    let specs = known_tool_specs();
    if specs.len() < 40 {
        return Err(TestError::Unexpected(format!(
            "only {} specs: the lint would pass vacuously",
            specs.len()
        )));
    }
    let mut violations: Vec<String> = Vec::new();
    let mut allowlisted_but_clean: Vec<&str> = Vec::new();
    for spec in specs {
        let issues = lint_tool_spec(spec);
        let allowlisted = ALLOWLIST.iter().any(|(tool, _)| *tool == spec.name());
        match (issues.is_empty(), allowlisted) {
            (false, false) => violations.push(format!("{}: {issues:?}", spec.name())),
            (true, true) => allowlisted_but_clean.push(spec.name()),
            _ => {}
        }
    }
    if !violations.is_empty() {
        return Err(TestError::Unexpected(format!(
            "tools violate the registry lint (description >= {MIN_DESCRIPTION_CHARS} chars, every \
             argument documented): {violations:#?}"
        )));
    }
    if !allowlisted_but_clean.is_empty() {
        return Err(TestError::Unexpected(format!(
            "stale allowlist entries (the tools pass now): {allowlisted_but_clean:?}"
        )));
    }
    Ok(())
}

#[test]
fn allowlist_entries_name_registered_tools_and_give_a_reason() -> TestResult {
    for (tool, reason) in ALLOWLIST {
        if !known_tool_specs().iter().any(|spec| spec.name() == *tool) {
            return Err(TestError::Unexpected(format!(
                "allowlist names {tool}, which no profile registers"
            )));
        }
        if reason.trim().len() < 10 {
            return Err(TestError::Unexpected(format!(
                "allowlist entry {tool} needs a reason"
            )));
        }
    }
    Ok(())
}

/// PL-97 B2 (Plan-Test): der Lint schlägt bei einem Fixture-Werkzeug mit
/// Zehn-Zeichen-Beschreibung an.
#[test]
fn lint_rejects_a_fixture_tool_with_a_ten_char_description() -> TestResult {
    let short = fixture("Reads it.!", Some("Workspace-relative path of the file."));
    let issues = lint_tool_spec(&short);
    if issues != [LintIssue::ShortDescription { chars: 10 }] {
        return Err(TestError::Unexpected(format!("{issues:?}")));
    }
    let good = fixture(
        "Reads a UTF-8 text file from the workspace and returns its content with line numbers.",
        Some("Workspace-relative path of the file."),
    );
    let issues = lint_tool_spec(&good);
    if !issues.is_empty() {
        return Err(TestError::Unexpected(format!("{issues:?}")));
    }
    Ok(())
}

#[test]
fn lint_rejects_an_undocumented_argument_field() -> TestResult {
    let long =
        "Reads a UTF-8 text file from the workspace and returns its content with line numbers.";
    for field_description in [None, Some("   ")] {
        let issues = lint_tool_spec(&fixture(long, field_description));
        if issues != [LintIssue::UndocumentedField("path".to_owned())] {
            return Err(TestError::Unexpected(format!(
                "{field_description:?}: {issues:?}"
            )));
        }
    }
    Ok(())
}
