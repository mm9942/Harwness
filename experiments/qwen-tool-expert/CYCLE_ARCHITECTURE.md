# Harwness Cycle Micro-Experts → Qwen3-1.7B

> Status: experimental architecture and offline training scaffolding, **not trained / deployed**.
> Repository evidence pinned to `dev` `197a92e92d438bf6bf93b129bb964f4852686149` (2026-10-08).

## Hypothesis

A cluster of narrowly fine-tuned ~0.1B language models can learn **distinct decision functions** of the Harw agent turn loop with less computation per decision than a single general-purpose model. Their **verified action trajectories** can then be distilled into an on-device-capable `Qwen/Qwen3-1.7B` student (a model identifier, not a claim about exact physical parameter count).

This is two distinct products:

1. **Executable composition:** a deterministic Harw state machine invokes only the relevant specialist(s) and retains the full authority to admit/execute tool calls. Summing model parameter counts is *not* equivalent to a monolithic model of that size.
2. **Distilled student:** the single Qwen3-1.7B model imitates successful, schema-valid, runtime-verified complete tool cycles; this is **knowledge distillation**, not tensor concatenation or naive weight merging.

The first twelve expert roles use `HuggingFaceTB/SmolLM2-135M-Instruct` (Apache-2.0) as a candidate base because Qwen3 has no official 0.1B-size checkpoint among those verified here. Twelve **independent** 134.5M-weight copies would sum to about **1.614B nominal parameters**; that does **not** establish 1.614B-equivalent general ability. A shared backbone with separate LoRA adapters is likely a more economical implementation, and the Qwen student is a different checkpoint with its own architecture and tokenizer.

## Runtime alignment: unchanged authority

The implemented `harw-core/src/turn_loop.rs` already gathers context, loads instructions, invokes the provider, applies approval/handoff checks, executes tool calls, records ToolResults, and continues or stops. `docs/design/agent-tool-loop.md` documents the handling of parallel-safe tools, resumable approvals and child handoffs. **Do not replace this with a learned policy**.

| Phase | Specialist (pilot) | Output | Authoritative owner |
|---|---|---|---|
| 0. Understand | `intent` | tool vs no-tool suggestion | Runtime/tool availability |
| 1. Select | `tool_selection` | a currently offered name or null | Role-specific live registry |
| 2. Build | `argument_builder` | tool name and typed JSON arguments | `ToolSpec` + runtime validator |
| 3. Execute | **no model** | real `ToolOutput` | `ToolExecutor`, approval, sandbox |
| 4. Observe | `observation` | result usable/degraded/partial/error | Untrusted result, runtime trust labels |
| 5. Continue | `continuation` | next suggested tool or finish | `TurnControl` limits, stop/handoff |
| 6. Verify | `verification` | next verification recommendation | Executor outcomes, tests, runtime state |

`schema_repair`, `failure_recovery`, `approval_advisory`, `delegation_advisory`, `context_budget` and `finalization` are **reserved** until curated training/evaluation data exists. They are *not* automatically invoked on every turn.

### Proposed cycle

```text
User task ──> Harw runtime: build context, visible/admitted ToolSpecs
                 │
                 ├─ intent (135M) [optional]
                 └─ tool_selection (135M) ─> argument_builder (135M)
                                              │
                                 JSON validation + permission/approval
                                              │
                                  Runtime ToolExecutor (REAL)
                                              │
                             ToolResult (untrusted, ordered, persisted)
                                              │
                              observation / continuation (135M)
                                              │
                     ┌─ follow-up tool ─────────┴──── finish / escalate
                     │                                      │
                     └──────────── cycle ───────────────────┴──> Qwen 1.7B synthesis
```

This diagram is **proposed routing**, not code merged into Harw's Rust runtime. Since multiple tiny model calls can still be *slower* than a single larger inference, measure active stages and latency per successful task. Use selective invocation, not a 12-model waterfall.

### Security and failure guarantees

- Stage outputs are **advisory**, untrusted model-generated data.
- Validate tool name **against the actual current advertised set**, not a trained catalog. Use **current** JSON schema, sandbox scope, runtime approval and the exact persisted call id.
- Never let micro-models assert `user_approved`, change budget, grant capabilities, or fabricate execution results.
- The initial examples have simulated tool results. A true training-success label needs executed tests against a disposable sandbox and a separate review/approval procedure.
- File edits, tool outputs, network responses and retrieved documents are untrusted observations, never instructions.
- When any micro proposal is invalid or insufficiently supported, fall back to the ordinary Harw runtime/provider path or ask for clarification. Do not use confidence percentages as the sole security gate.

## Current experiment artifacts

- `tool_catalog.seed.json`: 8 manually source-reconciled training tool contracts; **not** a live registry export.
- `build_dataset.py` and `validate_dataset.py`: starter whole-cycle conversations.
- `micro_experts.json`: twelve candidate specialist roles (six active in phase 0).
- `build_micro_dataset.py`: deterministic role-specific JSON-label datasets, using the same **family-held-out** tool tasks.
- `validate_micro_dataset.py`, `test_micro_dataset.py`: source-spec checks and no-leakage checks.
- `train_micro.py`: **opt-in** LoRA training for one 135M specialist adapter at a time.
- `train_qlora.py`: **opt-in** QLoRA starter for the Qwen3-1.7B target, using whole-cycle trajectories.

### Offline commands (no GPU or model download)

```sh
cd experiments/qwen-tool-expert
python3 build_dataset.py && python3 validate_dataset.py
python3 build_micro_dataset.py && python3 validate_micro_dataset.py
python3 -m unittest test_micro_dataset.py
python3 train_micro.py --expert intent  # dry-run only
python3 train_qlora.py                  # dry-run only
```

### Training, only after independent data review and a baseline

```sh
python3 train_micro.py --expert tool_selection --train
# Train each phase independently with reviewed data and test unseen tool schemas.
# Distil only successful, authorized, sandbox-verified cycles into Qwen3-1.7B.
python3 train_qlora.py --train
```

The present seed has too few examples for useful training; trainers deliberately refuse tiny data by default. Correct LoRA APIs, the Hugging Face chat-template assistant mask and production inference format **still require compatibility checks**. These scripts are research scaffolds, not verified GPU training jobs. **No cloud training is running.**

## Distillation contract (future wave)

An eligible teacher trajectory MUST include:

- `task_id`, `git_ref`, `role`, `tool_spec_hashes`, `allowed_tool_names`;
- model decisions from each phase, current tool schemas and args;
- **real** runtime tool results with matching call/result ids (not synthetic placeholders);
- approval, sandbox and failure metadata from trusted executor, not model predictions;
- objective outcome/evaluator results and provenance/redistribution consent.

Export only appropriately scrubbed and authorized records. Reject stale schemas, invalid arguments, rights violations, generated "results" without an executor, unpaired tool calls, unverifiable tasks, and leakage across train/evaluation families.

Use these trajectories as multi-turn conversational SFT for Qwen, with explicit tool-call/response boundaries; later preference optimization is optional and only if a reliable executable reward exists. Retain independent untouched evaluation tasks from newer Harw schema revisions.

## Acceptance targets (to measure, not claims)

1. At least as many **successful tool tasks per second** as untuned Qwen3-1.7B; compare costs, latency and total tokens, not only expert parameter counts.
2. Near-zero invalid tool names, unknown arguments and execution without appropriate runtime authority; always fail closed for high-risk cases.
3. Better task completion than an otherwise identical untuned baseline under **identical context/tool budgets**.
4. Verify streaming tool cycles, invalid schema recovery, `lens.ask` scope, exact-match edit failures, cancellation and handoffs.
5. Compare Qwen3-1.7B student `Q4_K_M` vs `Q5_K_M` GGUF on Samsung S24 Ultra with the same llama.cpp runtime and context. Measure real peak RAM/KV cache, prefill/generation latency, sustained thermal throughput and tool accuracy.

No deployment, GPU rental, billing changes or new authority is authorized by this document.
