# PL-96 / 06 — Scientific contribution, falsification and reproducibility

> **PLANNING ONLY.** No benchmark was run here. The existing [Harwness paper](../../research/harwness-paper.md) is pinned to main `90160f4f...`, while the PL-96 audit uses dev `090a9d2b...`. Do not silently move the manuscript baseline or claim performance improvements without measured artifacts.

## Research thesis and novelty boundary

The candidate contribution is **not** “Harwness has many agents/tools/memory”. Mature systems have variants of each. A stronger, falsifiable contribution is:

> A heterogeneous agent system can preserve one externally enforced, monotone authority and state lifecycle across model, agent, tool, execution, session and knowledge boundaries, while using small context/tool projections to improve work quality per unit cost.

The paper needs to show that **composition of boundaries**, not a feature count or repository size, improves correct end-to-end behavior. Any claim of uniqueness must be separately supported by a literature survey; no “first”, “only” or “superior” claim without independent evidence.

## Upgrade the claim matrix

Existing paper §31 uses coarse labels like “implemented memory”. For publication, attach per-capability states:

- Contract declared (static type/design);
- Implementation built;
- Production entry composed;
- Admission and denial tested;
- Effect observed, durable and recovered;
- Independently verified on exact commit;
- Deployed (when applicable).

Record negative/null findings, even when an architecture concept looks elegant. Compare paper main baseline and dev-audit delta side-by-side; neither supersedes the other. Record versioned experiment package SHA, exact Cargo.lock digest, service manifests and explicit missing `harw-cloud` paths before claiming a clean reproduction.

## Experiments (proposal matrix)

| ID | Hypothesis | Baseline vs treatment | Primary outcome | Falsifier / competing explanation |
|---|---|---|---|---|
| X1 Capability containment | Runtime-enforced authority reduces forbidden effects | Prompt-only scoped instructions vs schema-only vs authoritative runtime | Unauthorized successful effects / attempts | Any unauthorized effect in treated system |
| X2 Native tool interface | Typed discoverable tools reduce errors/overhead | Governed shell vs 42 typed tools, same task corpus | Verified completion, invalid-call rate, token use, latency | No quality/efficiency gain at equal permissions |
| X3 Context parity | Provider adapter preservation avoids information loss | Before/after Anthropic `data_block` fix + same logical fixture | Exact admitted fragment coverage; task success | Any required admitted data omitted on wire |
| X4 Skill injection | Deterministic event-triggered snippets improve targeted tasks | Pull-only skills vs S3 bounded auto-injection | Tool retry/errors, success/task, extra tokens | Trigger noise costs more than quality gained |
| X5 Crash-safe delegation | Durable job-backed agent runs reduce repeated work | Transcript-only resume vs JobStore/leases/fencing | duplicate side effects, recovery loss, time | Duplicate execution after restart |
| X6 Scoped knowledge | Governed promotion improves cross-session recall safely | Full history, summary-only and scoped retrieval | Factual precision, retrieval cost, ACL violations | Unauthorized private artifact retrieved |
| X7 Multi-surface equivalence | Equivalent effect checks stay consistent across entry points | TUI, one-shot, job, Web, MCP, Telegram, remote | Denial parity by expected EntryKind delta | A forbidden action is admitted on one surface |
| X8 Cache and concurrency | Affinity + bounded caches/lanes improve success/cost | Fixed lane/no cache vs controlled policies | successful jobs/sec, billable cost/success, 429 rates | Cross-tenant cache reuse, retry storm or worse cost |
| X9 Warden TCB | Signed v2 enforces proof authenticity/replay protection | v1 vs v2 in disposable test environment | unauthorized enforcement count, replay successes | Any forged/stale/replayed proof executes |
| X10 Agent specialization | Capability-directed agent selection beats unfiltered parallel fan-out | Unrestricted available roster vs scoped specialist selection | success per CPU/token, conflicting edits, review misses | More collisions/cost without quality improvement |
| X11 Semantic patterns | Provenance-preserving patterns improve human/agent diagnosis | raw event stream vs bounded activity projections | time to locate failure, mistaken status, missed event | Evidence loss or collapsed failures |
| X12 Review governance | SHA-bound wave gates reduce integration regressions | Broad PR merges vs small owned waves | post-merge regressions, mean rollback, conflicts | No measurable improvement, more reviewer overhead |

Experiments must distinguish **implementation wins** (proof constraints hold) from **model wins** (task success). Neither can be deduced from type existence or markdown planning.

## Controls, methods and confounders

1. Freeze exact repository/head and test corpus before trials; record runner OS/CPU/RAM, model IDs/weights/quantization and provider route/settings. Do not expose keys.
2. Keep model, task corpus, granted scope, temperature/effort and tool output caps fixed unless those variables are the planned treatment.
3. Use paired tasks with randomized order; multiple seeds where providers support them; independent reference grader blinded to condition.
4. Include negative, conflicting, oversized, untrusted and incomplete inputs, plus long-running “busy but meaningful” sessions.
5. Report denominators, N, missing/skipped runs, median/p95 latencies, confidence intervals/paired uncertainty, rate limits and total/billable tokens separately.
6. Show unit costs **per verified success**, not only per raw token; track cache input/writes/reads and full-result cache hits independently.
7. Label all vendor model behaviors as time/version-specific. No invented benchmark data from paper hypotheses.
8. Privacy: synthetic/redacted audit fixtures, no child raw private transcripts in public experiment exports; explicit independent approval of any real trajectories used for distillation.

### X2 detailed sample tasks

- File exploration: enumerate tree and inspect a bounded Rust file without escaping workspace.
- Git: detect tracked/untracked/staged changes; compare diff modes on checked fixture repository.
- Cargo: inspect metadata, fmt and test selected package in permitted sandbox, including tool unavailable/denied.
- Process: list target job/process read-only, with no backdoor to host control.
- Critic: locate exactly one planted bug with evidence rather than tool-output length.
- Long-turn: stream tool outputs, compaction, changed permissions and cancellation mid-call.

Equalize semantics and error handling; an inability of the new `gitread` implementation to handle certain GNU Git features must be recorded as unsupported, not scored as an agent reasoning failure.

### X7 full policy equivalence

For the same abstract operation, enumerate expected `EntryKind` ceilings. A difference explained by explicitly narrower channel policy is correct; an unexpected wider path is a failure. Include /op aliases and nested Telegram commands, direct ToolPort call, denied project layer, conflicting model instruction and job-worker continuation.

### X9 Warden proof experiment

Use recording/dummy executors or disposable Linux test namespaces. Never test by invoking destructive privileged actions against a real machine. Include signed proof tampering, replay after restart, changed cgroup and version negotiation. Check that the **actual privileged handler** rejects v1, not merely that the new v2 protocol's unit-test verifier rejects it.

## Artifact record (suggested schema; not yet implemented)

```json
{
  "schema": "harwness.research-run/v2",
  "repo_sha": "<40-hex>",
  "lockfile_digest": "<sha256>",
  "experiment_id": "X2-native-tools",
  "provider_route": "<versioned model route>",
  "config_digest": "<redacted immutable config digest>",
  "tool_registry_digest": "<admitted tools and schemas hash>",
  "agent_ir_digest": "<verified agent definition>",
  "scope_digest": "<rights and workspace>",
  "task_id": "fixture-001",
  "seed": null,
  "run_id": "<opaque unique id>",
  "independent_reviewer": "<opaque id>",
  "checks": [],
  "results": {
    "verified_success": null,
    "unauthorized_effects": null,
    "prompt_new_tokens": null,
    "prompt_cached_tokens": null,
    "cost_per_verified_success": null,
    "latency_ms": null
  },
  "status": "not_executed"
}
```

Null means unavailable/not measured, **not zero**. Attach test logs/checkpoints and recorded failure artifacts under controlled retention. A CSV/JSON export must never secretly replace original raw audit observations.

## Paper-specific implementation recommendations

- Keep the existing main-based paper frozen; make a separate dev-delta appendix after G1 clean-checkout and G4 gates succeed.
- Replace monolithic “Implemented” status labels with component/entry/verification coverage.
- Add methods X2, X8, X12 and full context parity as explicit novel experimental contributions, not just generic code audit.
- Strengthen comparison to durable workflow engines, MCP, agent SDKs and constrained sandbox runtimes; present alternative explanations fairly.
- Include limitations of a single-author/high-parallel-agent repo, self-review bias, privately configured gateway capacity, model drift, Linux-vs-Darwin differences, opaque vendor tool behavior and deployment-vs-code gap.
- Quote no fabricated observations from CI or model training; report exact run date when later measured.

**Acceptance for this research *plan*:** reviewers can distinguish claim, baseline, intervention, outcome, falsifier and artifact needed without running any experimental system.
