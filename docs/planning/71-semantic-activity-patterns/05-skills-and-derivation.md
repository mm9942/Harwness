# PL-71 / 05 — Patterns, Derivation and Skills

> Status: DRAFT  
> Parent: [README.md](README.md)

## Distinction

Current Harw skills are instruction fragments. Patterns are typed semantic structures.

```text
Skill:
  how to behave / what to consider

Pattern:
  what structure exists / was observed / can be derived
```

Neither carries authority.

## Why patterns are more linkable

A PatternDefinition has a stable ID, version, typed relations, matcher/reducer, provenance expectations and tests.

A skill is primarily model-readable instruction plus manifest.

Patterns are therefore a stronger substrate for linking and derivation.

## Skills remain valuable

Skills remain the right surface for concise model guidance, domain heuristics, procedures, examples, style rules and analytical framing.

The improvement is that a skill may reference stable patterns rather than independently restating all structure.

## Pattern-backed skill projection

```text
PatternGraph
  ↓ select accepted patterns
SkillProjectionBuilder
  ↓
instruction fragment
```

Example source patterns:

- file-edit-read-loop@1
- validation-after-mutation@1
- compile-fix-cycle@1

A projected instruction can explain the behavior in model-friendly prose while retaining links to the source patterns.

## Skill manifest compatibility

No immediate `SkillToml` schema change is required.

A later additive field might be:

```toml
patterns = [
  "file-edit-read-loop@1",
  "compile-fix-cycle@1",
]
```

This must be introduced deliberately because `SkillToml` uses `deny_unknown_fields`.

## Derivation graph

```text
file-edit-read-loop
  + validation-run
  + failure-diagnosis
        ↓
compile-fix-cycle
        ↓
implementation-iteration
```

Skills may choose which abstraction level they expose.

## AI-assisted discovery

AI is useful for clustering repeated sequences, suggesting candidate keys, finding counterexamples, proposing relation edges, generating explanations and proposing skill prose from accepted patterns.

AI is not the default final per-event matcher because replay and determinism matter.

## Proposal review

```text
runtime evidence
   ↓
pattern analyst
   ↓
PatternProposal
   ↓
validate on recorded traces
   ↓
review
   ↓
accepted PatternDefinition
```

## Deduplication of knowledge

Without shared patterns:

```text
Skill A says X
Skill B restates X
TUI hard-codes X
Agent monitor approximates X
```

With patterns:

```text
Pattern X
  ├─ referenced by Skill A
  ├─ referenced by Skill B
  ├─ rendered by TUI
  └─ consumed by analysis
```

## Safety

Pattern-derived skill prose does not inherit authority from the observations that produced it.

An observed command sequence does not become an allowed command sequence.

## Open ownership question

Accepted PatternDefinitions may ultimately live in Rust built-ins, TOML, a dedicated catalog, compiled artifacts or a layered combination. This remains open for implementation planning.

IMPLEMENTATION STATUS: planning only.
