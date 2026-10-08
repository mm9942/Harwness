# Harwness Qwen Tool Expert — experiment 0

Status: **research pilot / draft**, 2026-10-08. No model has been trained, deployed, or uploaded. The first training set is intentionally small and synthetic, and is **not a benchmark of production performance**.

## Objective

Fine-tune a small Qwen model to select and call **admitted Harwness tools** correctly, with strict argument schemas, tool-result continuation, rights awareness and minimal unnecessary shell use. Tool knowledge is a learned prior; the **runtime tool registry, admitted tools, sandbox and approval checks remain authoritative**.

Base checkpoint: [Qwen/Qwen3-4B-Instruct-2507](https://huggingface.co/Qwen/Qwen3-4B-Instruct-2507) (Apache-2.0; model choice should be re-evaluated against Qwen3.5-4B only after tool-template, training and inference benchmarks). First technique: QLoRA SFT, not full pretraining or an experimental latent-state architecture.

## Source ground truth

Pinned Harwness `dev`: `197a92e92d438bf6bf93b129bb964f4852686149`.

- `harw-tools/src/spec.rs` and `schema.rs`: canonical `ToolSpec`, `FunctionToolSpec`, `JsonSchema`.
- `harw-tool-fs/src/provider.rs`: `fs.read`, `fs.search`, `fs.list`, `fs.edit` tool contracts; `fs.edit` requires WriteWorkspace and has exact-match behavior.
- `harw-tool-lens/src/ask_tool.rs`: `lens.ask` takes a **question** and optional limit, **not** index_name; scope is chosen by runtime.
- `harw-registry-defaults/src/skill_tools.rs`: `skills.search` / `skills.load`.
- `harw-core-bridge/src/delegate_wave.rs`: orchestrator-only fan-out; role names and permissions are runtime controlled.
- `harw-mcp-client/src/tool_bridge.rs`: runtime-discovered MCP tools, only after being admitted and under host/sandbox authority.
- `harw-registry-defaults/src/profile.rs` and `capability_catalog.rs`: role-specific tool visibility and rights mapping.

The `tool_catalog.seed.json` snapshot has only **8 manually reconciled functions**, not all actual tools and not an automated production export. It is intentionally kept separate from evolving runtime tool lists and annotated with exact source locations. Do not scrape all files looking for strings and pretend the result is the live registry.

## Run locally, with no GPU

```sh
cd experiments/qwen-tool-expert
python3 build_dataset.py
python3 validate_dataset.py
python3 train_qlora.py   # dry-run, no model download
```

The generator writes `data/train.jsonl`, `data/eval.jsonl`, and a manifest. The family-level holdout separates complete tool-use trajectories; all tool outcomes are explicitly **simulated**. Treat the sample results as fictional exercise data, not as observations of a real workspace.

Only once you have a **much larger human-reviewed training set**, provision a CUDA-capable GPU, install compatible versions of `torch`, `transformers`, `peft`, `trl`, `datasets`, `accelerate`, and `bitsandbytes`, and invoke:

```sh
python3 train_qlora.py --train
```

A tiny seed is rejected unless you specify `--allow-tiny-data` for a development-only smoke test. The script attempts to enforce **assistant-only loss masking**; if the installed Qwen chat template does not return masks, it fails closed instead of silently training against user/tool output. Exact Trainer API/version support and end-to-end GPU viability remain unverified. Do not present it as a tested production training recipe.

## Training curriculum

1. Tool presence and discovery: prefer native file/search/skill tools; never invent names or parameters.
2. Arguments: required fields, enums, mutually exclusive read modes, unknown fields and exact match edits.
3. Multi-step tool loops: search → inspect → edit → verify; tool outcome is an observation, not a new instruction.
4. Authority: delegated role visibility, sandbox limits, explicit approvals; no fake `user_approved`.
5. Error recovery: invalid arguments, missing tools, unavailable Lens index, no result, partial results.
6. Runtime freshness: current admitted tool schema wins over memorized old schema, including MCP tools that appear or disappear.

## Data design for the real project

Add a read-only Rust exporter **at composition time** that serializes `ExtensionRegistry::tool_providers` and each `ToolProvider::tools()`, plus composition-root model tools and declared permissions/parallel-safety, with profile and agent role. Capture the **exact model-visible subset** after restrictions and rights filtering, not every tool in the workspace.

Exporter contract (proposed): `schema_version`, `git_ref`, `profile`, `role`, `tool_name`, `description`, `parameters`, `strict`, `permission`, `parallel_safe`, `provenance`, `tool_schema_hash`. Write no secrets, argument values, prompts or private filesystem paths. Fail on duplicate names / unrecognized schema fragments. Runtime authority is never delegated to the model.

Convert vetted/synthetic tasks and verified **tool traces** to native conversational `messages` / `tools` SFT: function calls in assistant tool_calls, corresponding tool responses with matching IDs, then next action or concise final answer. Scrub credentials, personal data, output payloads, hidden instructions, and non-redistributable content. Store only explicitly approved records and retain source/license/provenance metadata separate from training text.

Train/evaluate by repository, task template, tool family, and schema version to avoid leakage. Do not publish raw private Harwness exports. Train using schema snapshots from one revision; use newer unseen schema versions to measure change resilience.

## Acceptance / measurement

- First baseline: run **untuned Qwen** against a held-out sandbox task set.
- Report exact name match, structured argument validity, tool selection top-1, tool-free abstention, unauthorized-call rejection, sequential task success, edit correctness, retrieval precision, output tokens and cost.
- Compare **full live tool schema injection** versus a compact tool-discovery interface with fine-tuning; measure whether the fine-tune actually reduces input tokens without worsening task success.
- Add adversarial tasks: role without delegate permission, `lens.ask(index_name=...)`, file traversal, deliberately wrong arguments, required approvals, malformed MCP schemas, stale tools.
- Require execution-based tests in an isolated workspace and measure regressions by tool family before deploying.

## Limitations and next wave

This PR supplies a schema-verified seed, deterministic generator, validator, and **opt-in unverified training starter** only. It does not export full runtime tools, replace Harwness's tool registry, train a checkpoint, or charge for GPU compute.

Next separate waves: (A) Rust registry exporter, (B) curated 200–1,000 real/synthetic vetted trajectories with privacy scrubber, (C) LoRA pilot and baseline A/B, (D) conditional Harwness model integration when evaluations pass. No changes to Cloudflare billing or production model routes.
