# PL-71 / 04 — Macro-level Activity Metadata

> Status: DRAFT  
> Parent: [README.md](README.md)

## Why macros

Tool authors know semantic facts that renderers should not rediscover from tool names.

Current `#[tool]` already centralizes name, permission, host extraction and parallel safety. Activity metadata is a natural additional declaration, provided it stays separate from authority and model-facing JSON schema.

## Proposed syntax

Illustrative only:

```rust
#[harw_macros::tool(
    name = "fs.read",
    permission = "read_workspace",
    parallel_safe,
    activity(
        domain = "filesystem",
        verb = "read",
        resource = "path",
        effect = "observe"
    )
)]
async fn read(
    context: &ToolExecutionContext,
    args: FsReadArgs,
) -> Result<ToolOutput, FsError> {
    ...
}
```

For edit:

```rust
activity(
    domain = "filesystem",
    verb = "edit",
    resource = "path",
    effect = "mutate"
)
```

## Generated seam

The macro could generate an internal descriptor and resource-key extractor. Exact trait and crate ownership remain open until dependency review.

## Do not overload ToolSpec

`FunctionToolSpec` is provider/model-facing.

Internal activity metadata may need:

- resource extraction;
- reducer family;
- lifecycle class;
- effect category;
- projection hints.

Prefer a sibling internal descriptor over expanding ToolSpec indiscriminately.

## Static facts vs dynamic values

Macro declaration provides static schema:

```text
resource field = path
verb = read
```

Runtime extraction provides dynamic value:

```text
path = "src/lib.rs"
```

The macro must not perform filesystem I/O or authority checks merely to compute an activity key.

## Fail-closed semantics

If resource extraction fails:

- tool execution follows existing validation and authority behavior;
- activity grouping falls back to standalone/unkeyed observation;
- the matcher does not invent a key.

Activity metadata failure must never make tool execution less safe.

## Effects vocabulary

Initial descriptive effect categories:

```text
Observe
Mutate
Execute
Communicate
Create
Delete
Control
Other
```

These do not replace `Permission`. They answer a different question.

## Operations

`#[operation]` may later gain equivalent semantic metadata for job/control surfaces. Share vocabulary and meaning; do not force parser implementation sharing if that creates proc-macro coupling.

## Non-macro providers

Many current tools are hand-written. They need an equivalent descriptor API so macro adoption is not a prerequisite.

The macro is syntax plus compile-time validation, not the sole representation.

## Compile-time diagnostics

Future diagnostics should catch:

- unknown activity domain, verb or effect;
- resource references to nonexistent argument fields;
- incompatible key types;
- duplicate activity declarations;
- contradictions with hard static invariants.

## Generated documentation

Generated docs should state semantic domain, resource-key source, effect category and whether deterministic aggregation is supported.

They must explicitly say:

> Activity metadata is descriptive and does not grant authority.

## Migration

Start with filesystem tools visible in the motivating TUI case. Do not migrate all tools in one wave.

IMPLEMENTATION STATUS: planning only.
