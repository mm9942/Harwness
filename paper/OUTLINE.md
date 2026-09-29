# Harw Paper Outline

## 0. Working abstract

Modern language models are increasingly deployed inside agent harnesses that control context selection, tools, memory, permissions, persistence, and multi-agent execution. Yet many agent systems still treat these mechanisms as loosely coupled prompt wrappers. Harw explores a systems-oriented alternative: a typed, capability-bounded agent runtime in which context programs, agent roles, knowledge retrieval, durable memory, tool authority, session state, and verification are explicit runtime mechanisms.

This paper presents the architecture and an empirical evaluation of Harw. We study which agent responsibilities can be moved from implicit prompt adherence into deterministic or typed runtime structures, and measure the resulting effects on context efficiency, execution reliability, authority enforcement, recovery, and long-horizon task completion. Harw is implemented in Rust and separates model-dependent reasoning from model-independent runtime invariants. We release the implementation, experiment manifests, and reproducibility artifacts.

This abstract is provisional. It must be rewritten after the primary experiments are complete.

---

## 1. Introduction

Motivation:

- Agent quality is not a property of model weights alone.
- Context, memory, tools, authority and control flow increasingly determine observed behavior.
- Prompt-only control is difficult to inspect, test and enforce.
- Persistent agents create systems problems: recovery, identity, concurrency, permissions, replay and durable state.

Research question:

> Which agent responsibilities can be represented as explicit runtime structure rather than repeatedly inferred by the language model, and what are the measurable consequences?

Contributions should be stated only after the evidence ledger has enough support.

## 2. Background and Related Work

Organize by mechanism, not vendor:

- agent harnesses and tool-using agents;
- context engineering and compaction;
- external and long-term agent memory;
- skill/agent specification and typed intermediate representations;
- inference-time orchestration and multi-agent decomposition;
- capability systems, sandboxing and approval models;
- harness-level evaluation and trajectory collection.

Avoid claiming novelty merely because Harw combines known components. Novelty must come from a precise systems contribution, experimental result, or unusually strong integration/invariant.

## 3. Design Principles

### 3.1 Model is not the runtime

Separate:

- probabilistic reasoning,
- deterministic state transition,
- policy/authority,
- durable storage,
- retrieval,
- process/tool execution.

### 3.2 Context is a program, not a dump

Describe role-bound context programs and bounded retrieval.

### 3.3 Authority is data

Explain explicit principals, capabilities, narrowing, approvals and placement.

### 3.4 Durable state over conversational illusion

Sessions, diary, palace, goals, jobs, replay and recovery are persistent structures.

### 3.5 Planned mechanisms are not measured mechanisms

Explicitly state the paper's baseline commit and freeze rules.

## 4. Architecture

Suggested architecture figure:

```
                 ┌──────────────────────┐
                 │      Model API       │
                 └──────────┬───────────┘
                            │
                   model-dependent turn
                            │
        ┌───────────────────▼───────────────────┐
        │              Harw Runtime             │
        │                                       │
        │  Agent IR / Context Programs          │
        │  Session Host / Durable State         │
        │  Knowledge + Memory Retrieval         │
        │  Tool Gateway / Authority             │
        │  Jobs / Child Agents / Verification   │
        └───────┬──────────────┬────────────────┘
                │              │
         external tools    durable stores
```

### 4.1 Agent definitions and typed IR

Evidence candidates:

- embedded built-in agent definitions;
- context-program binding;
- IR lowering;
- local overlays vs trusted built-ins.

### 4.2 Knowledge and context

Evidence candidates:

- bounded recall;
- Diary;
- Palace;
- Dream;
- compaction;
- visibility boundaries.

### 4.3 Tool and execution authority

Evidence candidates:

- tool gateway;
- sandbox placement;
- delegation narrowing;
- approvals;
- no model-visible secrets.

### 4.4 Durable sessions and transport

Evidence candidates:

- session records;
- restart recovery;
- replay cursors;
- bounded fan-out/live ring;
- idempotent messages;
- approval arbitration.

### 4.5 Multi-agent execution and verification

Evidence candidates:

- child controller;
- role-specific workers;
- WorkDriver;
- goal contracts;
- wave manifests;
- deterministic gates.

## 5. Evaluation

The paper should have experiments, not screenshots.

### E1 — Context efficiency

Compare at least:

- unbounded/raw conversational context,
- Harw bounded retrieval,
- Harw bounded retrieval + memory/compaction.

Measure:

- prompt/input tokens,
- task success,
- retrieval precision/recall on a controlled corpus,
- latency,
- number of irrelevant artifacts injected.

### E2 — Runtime enforcement vs prompt-only instruction

Construct tasks where the model is instructed not to cross an authority boundary.

Compare:

- prompt-only restriction,
- Harw capability/placement enforcement.

Measure:

- attempted violations,
- executed violations,
- denial correctness,
- false denials,
- policy overhead.

The important metric is not whether a model *asks* for a forbidden action. The runtime should demonstrate that forbidden actions do not execute.

### E3 — Persistence and recovery

Inject failures:

- process restart,
- client disconnect,
- duplicate message,
- worker interruption,
- delayed consumer.

Measure:

- state loss,
- duplicated effects,
- successful replay,
- recovery latency,
- correctness after restart.

### E4 — Long-horizon engineering task

Use a fixed repository task suite.

Compare harness configurations while holding the base model constant where possible.

Potential ablations:

- no subagents,
- no durable memory,
- no goal/verification gate,
- full Harw runtime.

Measure:

- accepted patch rate,
- regression rate,
- tool calls,
- model tokens,
- wall time,
- recovery after interruption,
- number of human approvals.

### E5 — Provider/model portability

Run a subset of experiments across at least two model/provider families.

Purpose:

Show which measured properties belong to the harness and which depend strongly on model capability.

Do not present cross-provider results as perfectly controlled if APIs, reasoning modes or pricing differ.

## 6. Results

Keep primary tables small.

Suggested table classes:

- context cost/quality;
- security/authority enforcement;
- restart/replay correctness;
- long-horizon task success and cost.

Every table row must map to an experiment manifest.

## 7. Discussion

Topics:

- intelligence as weights + harness;
- what belongs in runtime vs training;
- benefits and costs of explicit state;
- deterministic structure around probabilistic reasoning;
- security and audit implications;
- limits of external memory;
- model/harness co-adaptation.

## 8. Limitations

Mandatory:

- Harw is one implementation;
- benchmark contamination/model training data cannot always be excluded;
- provider APIs are not identical;
- some runtime mechanisms may improve reliability while adding latency;
- model behavior remains stochastic;
- not all planned Harw architecture is implemented at the paper baseline;
- security evaluation does not prove absence of vulnerabilities.

## 9. Future Work

Keep unimplemented work here unless it lands and is evaluated before freeze:

- full Model-Turn-Chain runtime;
- adaptive model/effort allocation;
- automatic trajectory-to-training datasets;
- harness-policy learning;
- harness/model co-optimization;
- larger distributed cells/control plane experiments.

## 10. Reproducibility and Artifact Availability

Provide:

- exact Git commit;
- released Harw version;
- experiment manifests;
- scripts;
- machine/OS metadata;
- model names and date;
- model/provider settings that can legally be disclosed;
- raw aggregate results;
- evaluation code;
- artifact hashes.
