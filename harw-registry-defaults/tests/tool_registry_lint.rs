//! Registry-Lint (PL-97 B2, PL-88 (4)): jedes registrierte Werkzeug erklärt
//! sich und seine Argumente dem Modell.
//!
//! # Regel
//! Eine Stelle: `harw_registry_defaults::tool_index::lint_tool_spec` (mit
//! `lint_report` für die Ausnahmeliste). Die Beschreibung hat mindestens
//! `MIN_TOOL_DESCRIPTION_CHARS` (60) Zeichen, beginnt nach
//! `TOOL_DESCRIPTION_START_PATTERN` (`Use when` oder ein großgeschriebenes
//! Verb in der dritten Person, englisch `Reads`/`Returns`, deutsch
//! `Liest`/`Zeigt`), und jedes Argumentfeld der obersten Ebene hat eine nicht
//! leere Beschreibung. Dieselbe Funktion prüft in `harw-ops` die
//! Operations-Modellwerkzeuge.
//!
//! # Geltungsbereich
//! Hier: alle Werkzeuge, die irgendein `RegistryProfile` registriert
//! (`profile::known_tool_specs`, dieselben Provider wie
//! `profile_tool_providers`), plus die zustandslos baubaren Werkzeuge der
//! Composition-Root (`profile::root_tool_specs`: `skills.search`,
//! `skills.load`, `workbench.*`, `diary.read`, `palace.*`, `kanban.*`,
//! `memory.*` und die Autor- und Entscheidungswerkzeuge des Stewards,
//! `agents.write_*`, `agents.commit_proposal`, `skills.propose`, …). In
//! `harw-ops` (`tool_index_op.rs`,
//! `every_operation_model_tool_documents_itself_and_its_arguments`): jedes
//! Operations-Modellwerkzeug (`dod.*`, `tool.index`, `ps`, `status`,
//! `gateway.*`, `work_driver.*`, `intent_cycle.*`, …).
//!
//! Nicht geprüft, weil ihre Spezifikationen Laufzeitzustand brauchen und
//! kein Test sie ohne diesen bauen kann: `job.*` (`JobManager` legt sein
//! Zustandsverzeichnis an), `agents.build` (Job-Verdrahtung), die Spawn-,
//! Delegations-, Ergebnis- und Nachrichtenwerkzeuge der Laufzeit
//! (`delegate_wave`, `agents.catalog`, `agents.delegate`, `agent.*`,
//! `parent.message`, `work_driver.report`, `transfer_to_*`), `matrix.*`,
//! `plan.*`/`ask_user`, `host.sudo_exec`, `container.*` und `browser.*`.
//! Dieselbe Liste pinnt `harw-ops`
//! (`every_index_entry_has_a_summary_except_the_listed_runtime_tools`).
//!
//! # Ausnahmen
//! Werkzeuge, die die Regel heute verletzen, stehen in [`ALLOWLIST`] — je
//! Eintrag mit Grund. Die Regel selbst wird nicht abgeschwächt, und ein
//! Eintrag, dessen Werkzeug die Regel inzwischen erfüllt oder das niemand
//! mehr registriert, lässt den Test fehlschlagen (veraltete Ausnahme).

use std::collections::BTreeMap;

use harw_registry_defaults::profile::{known_tool_specs, root_tool_specs};
use harw_registry_defaults::tool_index::{
    MIN_TOOL_DESCRIPTION_CHARS, ToolSpecIssue, lint_report, lint_tool_spec,
};
use harw_tools::{FunctionToolSpec, JsonSchema, JsonSchemaType, ToolName, ToolSpec};

mod common;
use common::{TestError, TestResult};

/// Grund der Imperativ-Einträge: das dokumentierte Muster erkennt nur die
/// dritte Person (`Writes`, `Schreibt`), keinen Imperativ (`Write`).
const IMPERATIVE: &str = "beginnt mit einem Imperativ; TOOL_DESCRIPTION_START_PATTERN erkennt nur \
     die dritte Person (`Writes …`). Die Beschreibung gehört dem Werkzeug-Crate; Nachzug dort, \
     dann den Eintrag entfernen";

/// Werkzeuge, die die Regel heute verletzen: `(Werkzeug, Grund)`.
const ALLOWLIST: &[(&str, &str)] = &[
    (
        "fs.glob",
        "49 Zeichen (\"Findet Dateien im Workspace über ein Glob-Muster.\"); die \
         Beschreibung liegt in harw-tool-fs/src/glob.rs und ist im Golden-Test \
         harw-tool-fs/tests/golden/fs_tool_specs.json festgeschrieben — beides \
         außerhalb von PL-97 B2, Nachzug dort",
    ),
    // harw-tool-fs (Golden-Test harw-tool-fs/tests/golden/fs_tool_specs.json).
    ("fs.edit", IMPERATIVE),
    ("fs.write", IMPERATIVE),
    (
        "fs.search",
        "beginnt mit dem Adverb \"Recursively\"; Beschreibung in harw-tool-fs \
         (Golden-Test fs_tool_specs.json), Nachzug dort",
    ),
    // harw-tool-latex.
    ("latex.build", IMPERATIVE),
    ("latex.template", IMPERATIVE),
    // harw-tool-shell.
    ("shell.exec", IMPERATIVE),
    (
        "process.kill",
        "beginnt bewusst mit dem Warnpräfix \"DESTRUCTIVE:\" (harw-tool-process); \
         das Modell soll die Wirkung vor dem Verb lesen",
    ),
];

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
    let profile = known_tool_specs();
    let root = root_tool_specs();
    if profile.len() < 40 || root.len() < 10 {
        return Err(TestError::Unexpected(format!(
            "only {} profile and {} root specs: the lint would pass vacuously",
            profile.len(),
            root.len()
        )));
    }
    let problems = lint_report(profile.iter().chain(root), ALLOWLIST);
    if !problems.is_empty() {
        return Err(TestError::Unexpected(format!(
            "registry lint (description >= {MIN_TOOL_DESCRIPTION_CHARS} chars starting with \
             `Use when` or a verb, every argument documented; allowlist entries must be \
             current and give a reason): {problems:#?}"
        )));
    }
    Ok(())
}

/// PL-97 B2 (Plan-Test): der Lint schlägt bei einem Fixture-Werkzeug mit
/// Zehn-Zeichen-Beschreibung an.
#[test]
fn lint_rejects_a_fixture_tool_with_a_ten_char_description() -> TestResult {
    let short = fixture("Reads it.!", Some("Workspace-relative path of the file."));
    let issues = lint_tool_spec(&short);
    if issues != [ToolSpecIssue::ShortDescription { chars: 10 }] {
        return Err(TestError::Unexpected(format!("{issues:?}")));
    }
    if lint_report([&short], &[]).is_empty() {
        return Err(TestError::Unexpected(
            "the report must flag the fixture".to_owned(),
        ));
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
        if issues != [ToolSpecIssue::UndocumentedField("path".to_owned())] {
            return Err(TestError::Unexpected(format!(
                "{field_description:?}: {issues:?}"
            )));
        }
    }
    Ok(())
}

#[test]
fn lint_rejects_a_description_that_does_not_start_with_a_verb() -> TestResult {
    let issues = lint_tool_spec(&fixture(
        "Read-only view of a UTF-8 text file from the workspace with line numbers.",
        Some("Workspace-relative path of the file."),
    ));
    if issues
        != [ToolSpecIssue::NoLeadingVerb {
            start: "Read-only".to_owned(),
        }]
    {
        return Err(TestError::Unexpected(format!("{issues:?}")));
    }
    Ok(())
}
