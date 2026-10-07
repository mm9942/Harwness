---
id: DEC-INDEX
title: Design decisions and rationale
tags: [decision, index]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../guides/work-driver.md
---

# Design decisions and rationale

One note per decision, each with **Decision → Why → Consequences →
Where in the code**. The notes are Obsidian-compatible (YAML frontmatter as
properties, relative Markdown links for backlinks and graph) and remain
clickable on GitHub. The vault state (`.obsidian/`) is not versioned.

| ID | Decision |
| --- | --- |
| [DEC-001](DEC-001-passed-true.md) | Judge verdict `{"passed": bool}` — established test convention, fail closed |
| [DEC-002](DEC-002-judge-cheap-reads.md) | Judge as its own internal worker: reading is a cache read, only output costs |
| [DEC-003](DEC-003-provider-limits.md) | Provider TPM, concurrency and TOML limits are never exceeded |
| [DEC-004](DEC-004-no-parallel-builds.md) | Workers never build; one verification per workspace |
| [DEC-005](DEC-005-small-scopes-waves.md) | Smallest scopes, short-lived agents, many waves, warm caches |
| [DEC-006](DEC-006-model-agnostic.md) | Model-agnostic: no assumptions about tools and traces |
| [DEC-007](DEC-007-worker-rights.md) | Read everywhere, write only in your own scope |
| [DEC-008](DEC-008-no-tui.md) | No TUI for the work driver: job kind plus tool |
