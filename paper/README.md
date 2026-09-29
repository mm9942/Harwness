# Harw Research Paper Workstream

This directory tracks the research paper for Harw as a living, evidence-bound artifact.

## Working title

**Harw: A Typed, Capability-Bounded Runtime for Persistent Agentic Systems**

Alternative title for later review:

**Harw: Compiling Context, Authority, and Durable State into an Agent Harness**

## Paper thesis

The paper studies a systems approach to agent intelligence in which a substantial part of agent behavior is represented outside model weights as explicit runtime structure: typed agent/context definitions, bounded knowledge retrieval, durable memory surfaces, capability-constrained tool execution, persistent sessions, and deterministic verification/orchestration.

The paper must distinguish three classes of statements at all times:

- **Implemented / measured** — supported by code, tests, traces, or experiments pinned to a commit.
- **Designed / not yet measured** — present as a merged design or contract but not used as experimental evidence.
- **Future work** — proposed architecture, including any feature not demonstrated in a reproducible experiment.

The baseline for the initial paper workstream is:

```
mm9942/Harwness
main @ 442cf92e3e79f0a6fd31e650a55e8be676c47f1e
2026-09-29
```

## Intended contribution shape

The initial paper should make narrow, defensible claims around:

1. **Typed harness construction.**
   Built-in agent definitions and context programs are embedded at build time and lowered through typed agent IR instead of being treated only as unconstrained prompt text.

2. **Explicit context and memory management.**
   Harw uses bounded retrieval plus distinct memory/knowledge surfaces such as Diary, Palace, Dream, Workbench, and compaction rather than relying on an ever-growing transcript.

3. **Capability-bounded execution.**
   Tool execution, delegation, approvals, sandbox placement, and session ownership are explicit runtime concerns rather than informal prompt instructions.

4. **Durable agent runtime state.**
   Sessions, tool events, replay/recovery, goals, jobs, and agent state are designed as persistent runtime state that can survive beyond a single model call.

5. **Harness-level verification and observability.**
   The system makes orchestration decisions, failures, tool placement, and verification available as inspectable runtime artifacts.

The Model-Turn-Chain design, recursive synthesis, and future harness-to-training feedback loop should be presented as **future work unless implementation and experiments land before the paper freeze**.

## Target artifacts

The paper workstream should eventually contain:

```
paper/
├── README.md
├── OUTLINE.md
├── EVIDENCE_LEDGER.md
├── PUBLISHING.md
├── src/
│   ├── main.tex
│   ├── references.bib
│   └── sections/
├── figures/
├── experiments/
│   ├── manifests/
│   ├── scripts/
│   └── results/
└── artifacts/
    ├── paper.pdf
    └── source.tar.gz
```

Generated PDFs and experiment results must be reproducible from pinned source revisions. Large raw traces should not be committed to the source repository unless deliberately curated; publish them as a dataset/artifact repository instead.

## Research workflow

Every empirical claim should have:

- a claim ID in `EVIDENCE_LEDGER.md`,
- a pinned Harw commit,
- a reproducible command or script,
- machine/runtime metadata,
- model/provider metadata where relevant,
- raw or summarized measurements,
- an interpretation,
- stated limitations.

A paper draft must not turn planning documentation into empirical evidence.

## Publication targets

The intended public chain is:

```
GitHub source + reproducibility artifacts
        ↓
canonical preprint (arXiv when ready)
        ↓
Hugging Face artifact repository
        ↓
Hugging Face Paper Page linked through the arXiv ID
```

Hugging Face should mirror useful research artifacts and link back to the canonical GitHub source and preprint rather than become an independent, divergent copy of the paper.
