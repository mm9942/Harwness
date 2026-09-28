---
id: DEC-INDEX
title: Design-Entscheidungen und Gründe
tags: [decision, index]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../guides/work-driver.md
---

# Design-Entscheidungen und Gründe

Eine Notiz pro Entscheidung, jeweils mit **Entscheidung → Warum → Folgen →
Wo im Code**. Die Notizen sind Obsidian-kompatibel (YAML-Frontmatter als
Properties, relative Markdown-Links für Backlinks und Graph) und bleiben auf
GitHub klickbar. Der Vault-Zustand (`.obsidian/`) wird nicht versioniert.

| ID | Entscheidung |
| --- | --- |
| [DEC-001](DEC-001-passed-true.md) | Judge-Urteil `{"passed": bool}` — etablierte Testkonvention, fail closed |
| [DEC-002](DEC-002-judge-cheap-reads.md) | Judge als eigener interner Worker: Lesen ist Cache-Read, nur Ausgabe kostet |
| [DEC-003](DEC-003-provider-limits.md) | TPM-, Concurrency- und TOML-Grenzen der Provider werden nie überschritten |
| [DEC-004](DEC-004-no-parallel-builds.md) | Worker bauen nie; eine Verifikation je Workspace |
| [DEC-005](DEC-005-small-scopes-waves.md) | Kleinste Scopes, kurzlebige Agenten, viele Wellen, warme Caches |
| [DEC-006](DEC-006-model-agnostic.md) | Modellagnostisch: keine Annahmen über Tools und Traces |
| [DEC-007](DEC-007-worker-rights.md) | Lesen überall, Schreiben nur im eigenen Scope |
| [DEC-008](DEC-008-no-tui.md) | Kein TUI für den work driver: Job-Art plus Werkzeug |
