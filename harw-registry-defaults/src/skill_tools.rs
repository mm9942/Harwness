//! `SkillCatalogToolProvider` — die lesenden Katalog-Werkzeuge für Skills:
//! `skills.search` und `skills.load` (Plan R9, Teil A).
//!
//! # Verantwortung
//! Agenten **finden** Skills ausschließlich über `skills.search` und
//! **laden** sie vor der Arbeit mit `skills.load` — nie über das Dateisystem
//! (das Profil unter `~/.harw` ist aus der Sandbox heraus ohnehin nicht
//! erreichbar). Beide Werkzeuge lesen nur den bei der Montage eingefrorenen
//! [`SkillIndex`] (vertraute Config-Layer plus eingebettetes Bündel als
//! Rückfall, siehe `harw_catalog::SkillIndex`).
//!
//! - `skills.search {query?, limit?, offset?}` — rangierte Liste aus Name,
//!   erstem Satz der Beschreibung (≤ 160 Zeichen) und Quelle. Leere Anfrage:
//!   alle Skills alphabetisch, seitenweise (`offset`).
//! - `skills.load {name, section?}` — der vollständige Text, gerahmt wie ein
//!   fest injizierter Skill (`# Skill: <name> (sha256 …)`), gedeckelt auf
//!   64 KiB. Mit `section` nur dieser `##`-Abschnitt; über dem Deckel die
//!   Abschnittsliste. Ein unbekannter Name nennt die fünf ähnlichsten.
//!
//! # Rechteklasse
//! Keine Sandbox-Rechteklasse ([`crate::authority::tool_permission`] ist
//! `None`, wie bei `agent.result`): die Werkzeuge laufen im Host-Prozess,
//! lesen nur den Katalog und berühren weder Workspace noch Netz noch
//! Prozesse. Ein Skill verleiht keine Rechte — seine deklarierten Werkzeuge
//! sind nur Text. Beide stehen in [`crate::AUTO_APPROVED_TOOLS`] und nie in
//! [`crate::ALWAYS_ASK_TOOLS`]; registriert werden sie von der
//! Composition-Root (Wurzel und Kind-Registries,
//! [`crate::profile::skill_catalog_tools_for_role`]), nie über ein
//! [`crate::RegistryProfile`].
//!
//! # Nebenläufigkeit
//! `Send + Sync`; hält nur ein `Arc<SkillIndex>`.
//!
//! # Fehler
//! Kein Aufruf gibt `Err` zurück: ungültige Argumente und unbekannte Namen
//! erscheinen als [`ToolOutput::error`].

use std::collections::BTreeMap;
use std::sync::Arc;

use harw_catalog::{MAX_SKILL_LOAD_BYTES, SkillIndex, SkillIndexEntry};
use harw_extension_api::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
    ToolSpec,
};
use harw_tools::args::parse_args_null_as_object;
use harw_tools::schema_helpers::{object_schema, property};
use harw_tools::{FunctionToolSpec, JsonSchemaType, Permission};
use serde::Deserialize;

/// Name des Such-Werkzeugs.
pub const SKILLS_SEARCH: &str = "skills.search";

/// Name des Lade-Werkzeugs.
pub const SKILLS_LOAD: &str = "skills.load";

/// Vorgabe für `limit` bei einer Suchanfrage.
const DEFAULT_QUERY_LIMIT: usize = 10;

/// Vorgabe für `limit` bei der Gesamtliste (leere Anfrage).
const DEFAULT_LIST_LIMIT: usize = 30;

/// Obergrenze für `limit`.
pub const MAX_SEARCH_LIMIT: usize = 30;

/// Anzahl der Namensvorschläge bei einem unbekannten Skill.
const CLOSEST_NAMES: usize = 5;

/// Die einzeilige Kontextzeile für den Prompt (Plan R9, Teil A): nennt die
/// Anzahl verfügbarer Skills und beide Werkzeuge.
///
/// # Beispiele
/// ```rust
/// use harw_registry_defaults::skill_tools::skill_catalog_hint;
///
/// let hint = skill_catalog_hint(61);
/// assert!(hint.starts_with("61 Skills verfügbar"));
/// assert!(hint.contains("skills.search") && hint.contains("skills.load"));
/// assert!(!hint.contains('\n'));
/// ```
#[must_use]
pub fn skill_catalog_hint(count: usize) -> String {
    format!(
        "{count} Skills verfügbar — `{SKILLS_SEARCH}` findet passende, `{SKILLS_LOAD}` lädt \
         sie vor der Arbeit."
    )
}

/// Die Katalog-Werkzeuge über einem eingefrorenen [`SkillIndex`].
#[derive(Debug, Clone)]
pub struct SkillCatalogToolProvider {
    index: Arc<SkillIndex>,
}

impl SkillCatalogToolProvider {
    /// Baut den Provider über dem Index der Montage.
    #[must_use]
    pub fn new(index: Arc<SkillIndex>) -> Self {
        Self { index }
    }

    /// Der zugrunde liegende Index.
    #[must_use]
    pub fn index(&self) -> &Arc<SkillIndex> {
        &self.index
    }
}

// Beide Katalog-Werkzeuge lesen nur und deklarieren bewusst keine Rechteklasse
// (siehe Moduldoku); sie sind parallelsicher.
harw_tools::tool_provider! {
    impl for SkillCatalogToolProvider as provider, parallel_safe: all {
        SKILLS_SEARCH => {
            spec: search_spec(),
            executor: SkillCatalogExecutor { index: Arc::clone(&provider.index), tool: SkillTool::Search },
        },
        SKILLS_LOAD => {
            spec: load_spec(),
            executor: SkillCatalogExecutor { index: Arc::clone(&provider.index), tool: SkillTool::Load },
        },
    }
}

fn search_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "query".to_owned(),
        property(
            JsonSchemaType::String,
            "Stichwörter (Thema, Technik, Werkzeug). Leer: alle Skills alphabetisch.",
        ),
    );
    props.insert(
        "limit".to_owned(),
        property(JsonSchemaType::Integer, "Höchstzahl Treffer (1–30)."),
    );
    props.insert(
        "offset".to_owned(),
        property(
            JsonSchemaType::Integer,
            "Überspringt so viele Treffer (Blättern).",
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(SKILLS_SEARCH),
        description:
            "Findet Skills (Arbeitsanleitungen) nach Stichwörtern: Name, Kurzbeschreibung \
             und Quelle je Treffer. Einzige Art, Skills zu finden — nie im Dateisystem suchen. \
             Den Text lädt skills.load. Nur lesend."
                .to_owned(),
        parameters: object_schema(props, &[]),
        strict: true,
    })
}

fn load_spec() -> ToolSpec {
    let mut props = BTreeMap::new();
    props.insert(
        "name".to_owned(),
        property(JsonSchemaType::String, "Skill-Name aus skills.search."),
    );
    props.insert(
        "section".to_owned(),
        property(
            JsonSchemaType::String,
            "Optional: nur diesen ##-Abschnitt laden (Überschrift oder Teil davon).",
        ),
    );
    ToolSpec::Function(FunctionToolSpec {
        name: ToolName::new(SKILLS_LOAD),
        description: "Lädt einen Skill vollständig (höchstens 64 KiB) oder einen Abschnitt davon. \
             Vor der Arbeit laden und befolgen. Nur lesend."
            .to_owned(),
        parameters: object_schema(props, &["name"]),
        strict: true,
    })
}

/// Welches der beiden Werkzeuge ein Executor bedient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillTool {
    Search,
    Load,
}

struct SkillCatalogExecutor {
    index: Arc<SkillIndex>,
    tool: SkillTool,
}

impl ToolExecutor for SkillCatalogExecutor {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let arguments = call.arguments.clone();
        Box::pin(async move {
            Ok(match self.tool {
                SkillTool::Search => execute_search(&self.index, arguments),
                SkillTool::Load => execute_load(&self.index, arguments),
            })
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadArgs {
    name: String,
    #[serde(default)]
    section: Option<String>,
}

/// Kern von `skills.search` (testbar ohne Sandbox-Kontext).
fn execute_search(index: &SkillIndex, arguments: serde_json::Value) -> ToolOutput {
    let args: SearchArgs = match parse_args_null_as_object(SKILLS_SEARCH, &arguments) {
        Ok(args) => args,
        Err(output) => return output,
    };
    let query = args.query.unwrap_or_default();
    let listing = query.trim().is_empty();
    let default_limit = if listing {
        DEFAULT_LIST_LIMIT
    } else {
        DEFAULT_QUERY_LIMIT
    };
    let limit = args.limit.unwrap_or(default_limit);
    if limit == 0 || limit > MAX_SEARCH_LIMIT {
        return ToolOutput::error(format!(
            "{SKILLS_SEARCH}: limit muss zwischen 1 und {MAX_SEARCH_LIMIT} liegen"
        ));
    }
    let offset = args.offset.unwrap_or(0);
    let hits = index.search(&query);
    let total = hits.len();
    let skills: Vec<serde_json::Value> = hits
        .iter()
        .skip(offset)
        .take(limit)
        .map(|hit| {
            serde_json::json!({
                "name": hit.entry.name,
                "description": hit.entry.short_description(),
                "source": hit.entry.source.label(),
            })
        })
        .collect();
    let next_offset = offset.saturating_add(skills.len());
    let hint = if total == 0 {
        format!(
            "Kein Treffer. Andere Stichwörter versuchen oder mit leerer query alle {} Skills \
             auflisten.",
            index.len()
        )
    } else {
        format!("Lade einen passenden Skill vor der Arbeit mit {SKILLS_LOAD} {{\"name\": …}}.")
    };
    let mut result = serde_json::json!({
        "query": query.trim(),
        "total": total,
        "offset": offset,
        "skills": skills,
        "hint": hint,
    });
    if next_offset < total {
        result["next_offset"] = serde_json::Value::from(next_offset);
    }
    ToolOutput::json(result)
}

/// Kern von `skills.load` (testbar ohne Sandbox-Kontext).
fn execute_load(index: &SkillIndex, arguments: serde_json::Value) -> ToolOutput {
    let args: LoadArgs = match parse_args_null_as_object(SKILLS_LOAD, &arguments) {
        Ok(args) => args,
        Err(output) => return output,
    };
    let Some(entry) = index.get(&args.name) else {
        return ToolOutput::error(unknown_skill_message(index, &args.name));
    };
    match args
        .section
        .as_deref()
        .map(str::trim)
        .filter(|section| !section.is_empty())
    {
        Some(section) => load_section(entry, section),
        None => load_whole(entry),
    }
}

/// Der ganze Skill oder, über dem Deckel, Kopf plus Abschnittsliste.
fn load_whole(entry: &SkillIndexEntry) -> ToolOutput {
    let fragment = entry.fragment();
    if fragment.len() <= MAX_SKILL_LOAD_BYTES {
        return ToolOutput::text(fragment);
    }
    let mut text = format!(
        "# Skill: {} (sha256 {})\n{}\n\nDer Skill ist zu groß für einen Aufruf ({} Bytes, \
         Deckel {MAX_SKILL_LOAD_BYTES}). Lade ihn abschnittsweise mit {SKILLS_LOAD} \
         {{\"name\": \"{}\", \"section\": …}}. Abschnitte:\n",
        entry.name,
        entry.snapshot().sha256,
        entry.description.trim(),
        fragment.len(),
        entry.name,
    );
    for section in entry.sections() {
        text.push_str("- ");
        text.push_str(&section);
        text.push('\n');
    }
    ToolOutput::text(text)
}

/// Ein einzelner Abschnitt, ebenfalls auf den Deckel gekürzt.
fn load_section(entry: &SkillIndexEntry, section: &str) -> ToolOutput {
    let Some(fragment) = entry.section_fragment(section) else {
        let sections = entry.sections();
        let listed = if sections.is_empty() {
            "keine".to_owned()
        } else {
            sections.join(" | ")
        };
        return ToolOutput::error(format!(
            "{SKILLS_LOAD}: Skill '{}' hat keinen Abschnitt '{section}'. Abschnitte: {listed}",
            entry.name
        ));
    };
    if fragment.len() <= MAX_SKILL_LOAD_BYTES {
        return ToolOutput::text(fragment);
    }
    let mut cut = MAX_SKILL_LOAD_BYTES;
    while !fragment.is_char_boundary(cut) {
        cut -= 1;
    }
    ToolOutput::text(format!(
        "{}\n\n[… gekürzt auf {MAX_SKILL_LOAD_BYTES} Bytes]\n",
        &fragment[..cut]
    ))
}

/// Fehlermeldung für einen unbekannten Skill mit den ähnlichsten Namen.
fn unknown_skill_message(index: &SkillIndex, name: &str) -> String {
    let closest = index.closest_names(name, CLOSEST_NAMES);
    if closest.is_empty() {
        return format!(
            "{SKILLS_LOAD}: Skill '{name}' ist unbekannt; es sind keine Skills verfügbar."
        );
    }
    format!(
        "{SKILLS_LOAD}: Skill '{name}' ist unbekannt. Ähnlich: {}. Suche mit {SKILLS_SEARCH}.",
        closest.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_extension_api::contributors::ToolProvider;
    use serde_json::json;

    fn bundled_index() -> Arc<SkillIndex> {
        Arc::new(SkillIndex::build(&[]))
    }

    fn json_of(output: ToolOutput) -> TestResult<serde_json::Value> {
        match output {
            ToolOutput::Json { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("kein JSON: {other:?}"))),
        }
    }

    fn text_of(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Text { content } => Ok(content),
            other => Err(TestError::Unexpected(format!("kein Text: {other:?}"))),
        }
    }

    fn names(result: &serde_json::Value) -> Vec<String> {
        result["skills"]
            .as_array()
            .map(|skills| {
                skills
                    .iter()
                    .filter_map(|skill| skill["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn search_finds_the_pyramid_skill_for_pyramide_and_pyramidenprinzip() -> TestResult {
        let index = bundled_index();
        for query in ["Pyramide", "Pyramidenprinzip"] {
            let result = json_of(execute_search(&index, json!({ "query": query })))?;
            assert_eq!(
                names(&result).first().map(String::as_str),
                Some("business-writing-pyramid"),
                "{query}: {result}"
            );
            let first = &result["skills"][0];
            assert_eq!(first["source"], "eingebaut");
            let description = first["description"].as_str().unwrap_or_default();
            assert!(!description.is_empty() && description.chars().count() <= 161);
            assert!(
                result["hint"]
                    .as_str()
                    .is_some_and(|hint| hint.contains(SKILLS_LOAD)),
                "{result}"
            );
        }
        Ok(())
    }

    #[test]
    fn empty_query_pages_through_all_skills_alphabetically() -> TestResult {
        let index = bundled_index();
        let first = json_of(execute_search(&index, json!({})))?;
        let total = first["total"].as_u64().unwrap_or_default();
        assert_eq!(usize::try_from(total).ok(), Some(index.len()));
        let page = names(&first);
        assert_eq!(page.len(), DEFAULT_LIST_LIMIT.min(index.len()));
        let mut sorted = page.clone();
        sorted.sort();
        assert_eq!(page, sorted);
        let next = first["next_offset"]
            .as_u64()
            .ok_or(TestError::Missing("next_offset"))?;
        let second = json_of(execute_search(
            &index,
            json!({ "query": null, "limit": 5, "offset": next }),
        ))?;
        let second_page = names(&second);
        assert_eq!(second_page.len(), 5);
        assert!(
            page.last() < second_page.first(),
            "fortlaufend alphabetisch"
        );
        Ok(())
    }

    #[test]
    fn search_rejects_a_limit_outside_the_range() -> TestResult {
        let index = bundled_index();
        for limit in [0, 31] {
            let output = execute_search(&index, json!({ "query": "latex", "limit": limit }));
            assert!(matches!(output, ToolOutput::Error { .. }), "{limit}");
        }
        Ok(())
    }

    #[test]
    fn load_returns_the_framed_text_and_single_sections() -> TestResult {
        let index = bundled_index();
        let whole = text_of(execute_load(
            &index,
            json!({ "name": "business-writing-pyramid" }),
        ))?;
        assert!(whole.starts_with("# Skill: business-writing-pyramid (sha256 "));
        assert!(whole.len() <= MAX_SKILL_LOAD_BYTES);
        let entry = index
            .get("business-writing-pyramid")
            .ok_or(TestError::Missing("pyramid"))?;
        assert_eq!(whole, entry.fragment());

        let section = text_of(execute_load(
            &index,
            json!({ "name": "business-writing-pyramid", "section": "Checkliste" }),
        ))?;
        assert!(section.starts_with("# Skill: business-writing-pyramid (sha256 "));
        assert!(section.contains("## Checkliste"), "{section}");
        assert!(section.len() < whole.len());
        assert!(!section.contains("## Wann anwenden"), "{section}");

        let missing = execute_load(
            &index,
            json!({ "name": "business-writing-pyramid", "section": "gibt es nicht" }),
        );
        let ToolOutput::Error { message } = missing else {
            return Err(TestError::Unexpected("fehlender Abschnitt".to_owned()));
        };
        assert!(message.contains("Checkliste"), "{message}");
        Ok(())
    }

    #[test]
    fn load_of_an_unknown_skill_names_the_five_closest() -> TestResult {
        let index = bundled_index();
        let ToolOutput::Error { message } =
            execute_load(&index, json!({ "name": "business-writing" }))
        else {
            return Err(TestError::Unexpected("unbekannter Skill".to_owned()));
        };
        assert!(message.contains("business-writing-pyramid"), "{message}");
        assert!(message.contains(SKILLS_SEARCH), "{message}");
        let listed = message
            .split("Ähnlich: ")
            .nth(1)
            .and_then(|rest| rest.split(". ").next())
            .ok_or(TestError::Missing("Vorschlagsliste"))?;
        assert_eq!(listed.split(", ").count(), CLOSEST_NAMES, "{message}");
        Ok(())
    }

    #[test]
    fn oversized_skills_list_their_sections_instead() -> TestResult {
        let layer = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let dir = layer.path().join("skills").join("riesig");
        std::fs::create_dir_all(&dir).map_err(ctx("mkdir"))?;
        std::fs::write(
            dir.join("skill.toml"),
            "name = \"riesig\"\ndescription = \"Groß.\"\n",
        )
        .map_err(ctx("manifest"))?;
        let filler = "Zeile mit Inhalt.\n".repeat(3_000);
        std::fs::write(
            dir.join("instructions.md"),
            format!("## Teil A\n\n{filler}\n## Teil B\n\n{filler}\n## Teil C\n\nkurz\n"),
        )
        .map_err(ctx("instructions"))?;
        let index = Arc::new(SkillIndex::build(&[layer.path().to_path_buf()]));
        let overview = text_of(execute_load(&index, json!({ "name": "riesig" })))?;
        assert!(overview.len() < MAX_SKILL_LOAD_BYTES);
        for section in ["- Teil A", "- Teil B", "- Teil C"] {
            assert!(overview.contains(section), "{overview}");
        }
        let part = text_of(execute_load(
            &index,
            json!({ "name": "riesig", "section": "Teil C" }),
        ))?;
        assert!(part.contains("kurz"));
        Ok(())
    }

    #[test]
    fn provider_offers_both_tools_without_permission_class() -> TestResult {
        let provider = SkillCatalogToolProvider::new(bundled_index());
        let names: Vec<String> = provider
            .tools()
            .iter()
            .map(|spec| {
                let ToolSpec::Function(function) = spec;
                function.name.as_str().to_owned()
            })
            .collect();
        assert_eq!(names, SkillCatalogToolProvider::TOOL_NAMES);
        for tool in SkillCatalogToolProvider::TOOL_NAMES {
            let name = ToolName::new(*tool);
            assert!(provider.executor(&name).is_some(), "{tool}");
            assert!(provider.parallel_safe(&name), "{tool}");
            assert_eq!(crate::authority::tool_permission(tool), None, "{tool}");
            assert!(crate::AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
            assert!(!crate::ALWAYS_ASK_TOOLS.contains(tool), "{tool}");
        }
        assert!(
            provider
                .executor(&ToolName::new("skills.propose"))
                .is_none()
        );
        assert_eq!(
            SkillCatalogToolProvider::TOOL_PERMISSIONS.len(),
            SkillCatalogToolProvider::TOOL_NAMES.len()
        );
        Ok(())
    }
}
