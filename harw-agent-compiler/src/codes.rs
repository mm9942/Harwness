//! The compiler's own diagnostic codes (`HARW-BUILD-NNN`) and the combined
//! catalog lookup for `harw agent explain HARW-…`.
//!
//! `harw-agent-dsl` reserves the `BUILD` area for compiler backends
//! (`diagnostics.rs`, module docs); the codes live here, next to the passes
//! that emit them. Numbers are never reused.

use harw_agent_dsl::diagnostics::{Area, CATALOG, DiagnosticCode, Severity};

macro_rules! build_code {
    ($name:ident, $code:literal, $sev:ident, $title:literal, $help:literal, $example:literal, $fix:literal) => {
        #[doc = $title]
        pub const $name: DiagnosticCode = DiagnosticCode {
            code: $code,
            area: Area::Build,
            severity: Severity::$sev,
            title: $title,
            help: $help,
            example: $example,
            fix: $fix,
        };
    };
}

build_code!(
    ROLE_NOT_COMPILABLE,
    "HARW-BUILD-001",
    Error,
    "this role cannot be compiled into a standalone agent",
    "the agent steward only runs inside harw; compile workers, orchestrators, UIA workers or a user-interface agent",
    "role = \"agent-steward\"",
    "compile a `worker` or `child-orchestrator` definition instead"
);
build_code!(
    WORKER_DELEGATES,
    "HARW-BUILD-002",
    Warning,
    "a worker declares delegation targets",
    "workers cannot spawn agents; `[delegation] targets` has no effect on a worker",
    "role = \"worker\"\n\n[delegation]\ntargets = [\"explorer\"]",
    "remove `[delegation]`, or make the definition `role = \"child-orchestrator\"`"
);
build_code!(
    UNKNOWN_TOOL,
    "HARW-BUILD-003",
    Error,
    "no provider serves this tool",
    "a compiled agent can only use tools of the capability catalog; the help lists close names",
    "[tools]\nadmitted = [\"fs.reed\"]",
    "use a catalog tool name: `admitted = [\"fs.read\"]`"
);
build_code!(
    WIDENS_BASE_ROLE,
    "HARW-BUILD-004",
    Error,
    "the rights manifest exceeds the base role",
    "a definition may only narrow the rights of the built-in role it extends (tools, depth, budget, effort)",
    "extends = { id = \"harwness.agent.worker-base@1\" }   # generic worker: analyst ceiling\n\n[tools]\nadmitted = [\"fs.read\", \"shell.exec\"]",
    "drop the widening entry (`shell.exec`), or extend a built-in role whose ceiling admits it (e.g. `harwness.agent.executor@1`)"
);
build_code!(
    EXCEEDS_AUTHOR_CEILING,
    "HARW-BUILD-005",
    Error,
    "the rights manifest exceeds the author ceiling",
    "the build was asked to stay within an author ceiling (the rights of whoever requested the build); the manifest claims more",
    "# built by an agent whose own rights are read-only\n[tools]\nadmitted = [\"fs.write\"]",
    "remove the rights the author does not hold, or build as an author who holds them"
);
build_code!(
    BASE_ABOVE_CEILING,
    "HARW-BUILD-006",
    Note,
    "the base role holds rights the author ceiling does not",
    "the effective rights are min(manifest, base role, author ceiling); the difference is narrowed away",
    "# author ceiling: read-only; base role: executor (shell)\nextends = { id = \"harwness.agent.executor@1\" }",
    "nothing to fix; list only the rights the agent needs"
);
build_code!(
    SKILL_NOT_FOUND,
    "HARW-BUILD-007",
    Error,
    "a skill cannot be found in the skill layers",
    "skills are embedded at build time from the skill index (profile, project and bundled skills)",
    "skills = [\"evidence-quality-reveiw\"]",
    "use an installed skill name (`harw agent skills list`): `skills = [\"evidence-quality-review\"]`"
);
build_code!(
    SKILL_DISABLED,
    "HARW-BUILD-008",
    Error,
    "a skill is disabled in a layer",
    "a disabled skill is never embedded; enable it or remove it from `skills`",
    "# skills/review/skill.toml has enabled = false\nskills = [\"review\"]",
    "set `enabled = true` in the skill manifest, or remove the skill from `skills`"
);
build_code!(
    TOOL_PRUNED,
    "HARW-BUILD-009",
    Note,
    "a tool was pruned from the compiled agent",
    "the compiled agent cannot use it (forbidden twice, or a spawn tool without anything to spawn)",
    "role = \"worker\"\n\n[tools]\nadmitted = [\"fs.read\", \"agent.status\"]",
    "remove the tool from `tools.admitted`; the compiled agent never has it"
);
build_code!(
    PROVIDER_ENV,
    "HARW-BUILD-010",
    Note,
    "a required environment variable was added to the manifest",
    "the model provider (or the HTTP interface) needs a credential; the binary reads it from the environment at runtime",
    "[models]\nprovider = \"anthropic\"\nmodel = \"claude-sonnet-5\"",
    "list it yourself to silence the note: `required_env = [\"ANTHROPIC_API_KEY\"]`"
);
build_code!(
    NO_MODELS,
    "HARW-BUILD-011",
    Warning,
    "no `[models]` table",
    "without a model preference the runner uses its default provider and needs that provider's credential at runtime",
    "# no [models] table",
    "add `[models]` with `provider`, `model` and `required_env`"
);
build_code!(
    UNKNOWN_CHILD,
    "HARW-BUILD-012",
    Error,
    "a delegation target or child orchestrator cannot be resolved",
    "every agent a compiled orchestrator can start is embedded; each name must resolve to a definition",
    "[delegation]\ntargets = [\"rust-implemnter\"]",
    "fix the name, or install the definition: `targets = [\"rust-implementer\"]`"
);
build_code!(
    CHILD_EXCEEDS_PARENT,
    "HARW-BUILD-013",
    Error,
    "a child agent claims more than its parent can pass down",
    "a child's remaining depth, budget, effort and authority must fit inside the parent's",
    "# parent: [spawn] max_depth = 1, budget max_tokens = 50000\n# child:  [spawn.budget] max_tokens = 80000",
    "lower the child's budget to at most the parent's (`max_tokens = 50000`) or raise the parent's"
);
build_code!(
    CHILD_REPEATED,
    "HARW-BUILD-014",
    Note,
    "an agent is reachable more than once in the delegation graph",
    "it is embedded once; later occurrences reuse the same nested artifact",
    "# a → b, a → c → b",
    "nothing to fix"
);
build_code!(
    NO_BASE_ROLE,
    "HARW-BUILD-015",
    Error,
    "no built-in base role bounds this definition",
    "a compiled agent's rights are checked against the built-in role it extends; this role has none",
    "role = \"root-orchestrator\"\nextends = { id = \"acme.agent.custom-root@1\" }",
    "extend a built-in role or base (`harwness.agent.worker-base@1`, `harwness.agent.child-orchestrator-base@1`)"
);
build_code!(
    DEPTH_WITHOUT_CHILDREN,
    "HARW-BUILD-016",
    Warning,
    "an orchestrator has no delegation targets",
    "without `[delegation] targets` or `spawn.child_orchestrators` a compiled orchestrator can start nothing",
    "role = \"child-orchestrator\"\n# no [delegation]",
    "add `[delegation]` with `targets = [\"explorer\"]`"
);
build_code!(
    CHILD_CYCLE,
    "HARW-BUILD-017",
    Error,
    "a delegation chain embeds one of its own ancestors",
    "unlike a diamond (the same agent reachable through two different paths, `HARW-BUILD-014`), this name is already on the path from the compiled root down to here; embedding it would recurse forever",
    "# a → b → a",
    "remove the delegation back to the ancestor, or restructure the family so it is a DAG"
);

/// Every compiler code, in number order.
pub const BUILD_CATALOG: &[DiagnosticCode] = &[
    ROLE_NOT_COMPILABLE,
    WORKER_DELEGATES,
    UNKNOWN_TOOL,
    WIDENS_BASE_ROLE,
    EXCEEDS_AUTHOR_CEILING,
    BASE_ABOVE_CEILING,
    SKILL_NOT_FOUND,
    SKILL_DISABLED,
    TOOL_PRUNED,
    PROVIDER_ENV,
    NO_MODELS,
    UNKNOWN_CHILD,
    CHILD_EXCEEDS_PARENT,
    CHILD_REPEATED,
    NO_BASE_ROLE,
    DEPTH_WITHOUT_CHILDREN,
    CHILD_CYCLE,
];

/// Looks a code up in the DSL catalog and in [`BUILD_CATALOG`]
/// (case-insensitive).
#[must_use]
pub fn lookup_code(code: &str) -> Option<&'static DiagnosticCode> {
    let wanted = code.trim().to_ascii_uppercase();
    CATALOG
        .iter()
        .chain(BUILD_CATALOG)
        .find(|entry| entry.code == wanted)
}

/// `true` if `text` looks like a diagnostic code (`HARW-XXX-NNN`).
#[must_use]
pub fn looks_like_code(text: &str) -> bool {
    let upper = text.trim().to_ascii_uppercase();
    let mut parts = upper.split('-');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("HARW"), Some(area), Some(number), None)
            if !area.is_empty() && number.len() == 3 && number.chars().all(|c| c.is_ascii_digit())
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn test_build_codes_are_unique_numbered_and_documented() {
        let mut seen = BTreeSet::new();
        for (index, entry) in BUILD_CATALOG.iter().enumerate() {
            assert!(seen.insert(entry.code), "duplicate {}", entry.code);
            assert_eq!(entry.code, format!("HARW-BUILD-{:03}", index + 1));
            assert_eq!(entry.area, Area::Build);
            assert!(!entry.example.is_empty() && !entry.fix.is_empty());
        }
        for entry in CATALOG {
            assert!(
                !entry.code.starts_with("HARW-BUILD-"),
                "DSL defines no BUILD codes"
            );
        }
    }

    #[test]
    fn test_lookup_covers_both_catalogs() {
        assert_eq!(
            lookup_code("harw-patch-003").map(|entry| entry.code),
            Some("HARW-PATCH-003")
        );
        assert_eq!(
            lookup_code("HARW-BUILD-004").map(|entry| entry.code),
            Some("HARW-BUILD-004")
        );
        assert!(lookup_code("HARW-NOPE-001").is_none());
        assert!(looks_like_code("HARW-PATCH-003"));
        assert!(!looks_like_code("evidence-critic"));
    }
}
