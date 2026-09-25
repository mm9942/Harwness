//! `harw agent explain`: where an IR value comes from, why a tool is
//! admitted or forbidden, and what a diagnostic code means.

use serde::Serialize;
use serde_json::Value;

use crate::codes::lookup_code;
use crate::compiler::Compiled;
use crate::discovery::SourceSet;
use crate::graph::PATCH_OPS;
use harw_registry_defaults::capability_catalog;

/// The explanation of a diagnostic code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodeExplanation {
    /// Code.
    pub code: String,
    /// Severity.
    pub severity: String,
    /// Title.
    pub title: String,
    /// Default help.
    pub help: String,
    /// A definition fragment that triggers it.
    pub example: String,
    /// How to fix the example.
    pub fix: String,
}

impl CodeExplanation {
    /// Text form.
    #[must_use]
    pub fn text(&self) -> String {
        let example: String = self
            .example
            .lines()
            .map(|line| format!("    {line}\n"))
            .collect();
        format!(
            "{} ({}): {}\n\n  {}\n\nExample that triggers it:\n\n{example}\nFix: {}\n",
            self.code, self.severity, self.title, self.help, self.fix
        )
    }
}

/// Explains a diagnostic code; `None` for an unknown code.
#[must_use]
pub fn explain_code(code: &str) -> Option<CodeExplanation> {
    lookup_code(code).map(|entry| CodeExplanation {
        code: entry.code.to_owned(),
        severity: entry.severity.as_str().to_owned(),
        title: entry.title.to_owned(),
        help: entry.help.to_owned(),
        example: entry.example.to_owned(),
        fix: entry.fix.to_owned(),
    })
}

/// Where one field got its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Provenance {
    /// Definition ID that contributes.
    pub definition: String,
    /// Layer.
    pub layer: String,
    /// `file:line:column`.
    pub location: String,
    /// `set` or the patch operator.
    pub how: String,
}

/// The explanation of one field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldExplanation {
    /// The field path (`tools.admitted`, `spawn.max_depth`, …).
    pub field: String,
    /// The value in the compiled IR (JSON), if the path exists there.
    pub value: Option<Value>,
    /// Contributions in resolution order.
    pub provenance: Vec<Provenance>,
    /// Extra notes (tool admission, pruning, catalog class).
    pub notes: Vec<String>,
}

/// IR JSON paths that differ from the DSL key paths.
fn ir_path(field: &str) -> String {
    match field {
        "instructions_file" => "instructions.source".to_owned(),
        "delegation.targets" => "spawn.delegation_targets".to_owned(),
        other => other.to_owned(),
    }
}

fn lookup_json<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |current, key| current.get(key))
}

/// The top-level fields `explain <name>` lists without a field argument.
pub const DEFAULT_FIELDS: &[&str] = &[
    "role",
    "specialization",
    "instructions_file",
    "skills",
    "tools.admitted",
    "tools.forbidden",
    "spawn.max_depth",
    "spawn.budget",
    "delegation.targets",
    "context.program",
    "return.contract",
    "models",
    "binary.interfaces",
    "binary.default_interface",
];

/// Explains `field` of a compiled agent: its value, every definition in
/// the resolution chain that sets or patches it, and for a tool
/// (`tool:<name>` or `tools.<name>`) why it is admitted, forbidden or
/// pruned.
#[must_use]
pub fn explain_field(compiled: &Compiled, sources: &SourceSet, field: &str) -> FieldExplanation {
    if let Some(tool) = field
        .strip_prefix("tool:")
        .or_else(|| field.strip_prefix("tools.").filter(|rest| rest.contains('.') || rest.contains('_')))
        .filter(|tool| !matches!(*tool, "admitted" | "forbidden"))
    {
        return explain_tool(compiled, sources, tool);
    }
    let ir_json = serde_json::to_value(&compiled.unit.ir).unwrap_or(Value::Null);
    let value = lookup_json(&ir_json, &ir_path(field)).cloned();
    FieldExplanation {
        field: field.to_owned(),
        value,
        provenance: provenance(compiled, sources, field),
        notes: Vec::new(),
    }
}

/// Every definition in the resolution chain that sets or patches `field`.
#[must_use]
pub fn provenance(compiled: &Compiled, sources: &SourceSet, field: &str) -> Vec<Provenance> {
    let mut ids: Vec<String> = compiled
        .unit
        .ir
        .trace
        .steps
        .iter()
        .map(|step| step.source.clone())
        .collect();
    let target = compiled.unit.ir.id.to_string();
    if !ids.contains(&target) {
        ids.push(target);
    }
    let mut out = Vec::new();
    for id in ids {
        let Some(entry) = sources.find_id(&id) else {
            continue;
        };
        let Some(file) = sources.file(&entry.label) else {
            continue;
        };
        if let Some(span) = file.span_of(field) {
            out.push(Provenance {
                definition: id.clone(),
                layer: format!("{:?}", entry.layer),
                location: span.to_string(),
                how: "set".to_owned(),
            });
        }
        for op in PATCH_OPS {
            if let Some(span) = file.span_of(&format!("patch.{field}.{op}")) {
                out.push(Provenance {
                    definition: id.clone(),
                    layer: format!("{:?}", entry.layer),
                    location: span.to_string(),
                    how: format!("patch {op}"),
                });
            }
        }
    }
    out
}

fn explain_tool(compiled: &Compiled, sources: &SourceSet, tool: &str) -> FieldExplanation {
    let unit = &compiled.unit;
    let mut notes = Vec::new();
    let admitted = unit.ir.tools.admitted.iter().any(|t| t == tool);
    let forbidden = unit.ir.tools.forbidden.iter().any(|t| t == tool);
    match (admitted, forbidden) {
        (true, false) => notes.push(format!("`{tool}` is admitted and in the rights manifest")),
        (_, true) => notes.push(format!("`{tool}` is forbidden (`tools.forbidden`)")),
        (false, false) => notes.push(format!("`{tool}` is not admitted (not in `tools.admitted`)")),
    }
    if let Some((_, reason)) = unit.pruned.iter().find(|(pruned, _)| pruned == tool) {
        notes.push(format!("pruned by the compiler: {reason}"));
    }
    if let Some(flow) = &unit.rights {
        if let Some(base) = &flow.base {
            let in_base = base.rights.tools.contains(tool);
            notes.push(format!(
                "base role `{}` ({}) {} it",
                base.base_role,
                base.reason,
                if in_base { "admits" } else { "does not admit" }
            ));
        }
        if let Some(ceiling) = &flow.ceiling {
            notes.push(format!(
                "the author ceiling {} it",
                if ceiling.tools.contains(tool) { "admits" } else { "does not admit" }
            ));
        }
    }
    match capability_catalog::lookup(tool) {
        Some(entry) => notes.push(format!(
            "catalog: provider `{}` ({}), class {}, runner feature `{}`{}",
            entry.provider.id,
            entry.provider.crate_name,
            entry.class.as_str(),
            entry.feature(),
            if entry.always_available { ", always available" } else { "" }
        )),
        None => notes.push("catalog: no provider serves this tool".to_owned()),
    }
    let mut provenance = provenance(compiled, sources, "tools.admitted");
    provenance.extend(self::provenance(compiled, sources, "tools.forbidden"));
    FieldExplanation {
        field: format!("tool:{tool}"),
        value: Some(Value::Bool(admitted && !forbidden)),
        provenance,
        notes,
    }
}

/// Text form of field explanations.
#[must_use]
pub fn render_fields(name: &str, explanations: &[FieldExplanation]) -> String {
    let mut out = format!("{name}:\n");
    for explanation in explanations {
        let value = explanation
            .value
            .as_ref()
            .map_or_else(|| "(not in the IR)".to_owned(), Value::to_string);
        out.push_str(&format!("\n  {} = {value}\n", explanation.field));
        if explanation.provenance.is_empty() {
            out.push_str("    from: default (no definition in the chain sets it)\n");
        }
        for source in &explanation.provenance {
            out.push_str(&format!(
                "    {} in {} ({}) at {}\n",
                source.how, source.definition, source.layer, source.location
            ));
        }
        for note in &explanation.notes {
            out.push_str(&format!("    · {note}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_explain_code_has_example_and_fix() -> Result<(), &'static str> {
        let explanation = explain_code("HARW-PATCH-003").ok_or("HARW-PATCH-003 is in the catalog")?;
        assert_eq!(explanation.severity, "error");
        let text = explanation.text();
        assert!(text.starts_with("HARW-PATCH-003 (error):"), "{text}");
        assert!(text.contains("Example that triggers it:"), "{text}");
        assert!(text.contains("[patch.skills]"), "{text}");
        assert!(text.contains("Fix: "), "{text}");
        assert!(explain_code("HARW-BUILD-004").is_some());
        assert!(explain_code("HARW-NOPE-999").is_none());
        Ok(())
    }

    #[test]
    fn test_json_lookup_by_dotted_path() {
        let value = serde_json::json!({"a": {"b": [1, 2]}});
        assert_eq!(lookup_json(&value, "a.b"), Some(&serde_json::json!([1, 2])));
        assert_eq!(lookup_json(&value, "a.c"), None);
        assert_eq!(ir_path("instructions_file"), "instructions.source");
    }
}
