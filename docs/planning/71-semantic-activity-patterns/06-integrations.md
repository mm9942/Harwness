# PL-71 / 06 — Integrations

> Status: DRAFT  
> Parent: [README.md](README.md)

## TUI history

CURRENT: ToolCell mutates in place per ToolCallId and ToolGroupCell aggregates consecutive read-only tool cells.

TARGET: pattern-backed resource projections.

```text
● src/job.rs · Edit ×2 · Read ×7
  last read 340..420 · active
```

Detail mode reveals constituent calls.

## Agent panel

CURRENT: one AgentLive per concrete identity.

TARGET: keep concrete active/unseen important agents, while allowing compact aggregate views:

```text
✗ uia-writer ×3 · model request failed
```

Expanded view preserves each SessionId, timing and trace.

## Job panel

Job status, log and wait observations should update one lifecycle projection.

```text
● rust-tests · 12m04s · logs ×2 · status ×5
```

Terminal:

```text
✓ rust-tests · exit 0 · 14m51s
```

## Chains

Durable Reasoning/Generation chains from PL-69 can consume PatternInstances as structured observations.

```text
ReasoningChain
  sees compile-fix-cycle repeated 5 times
  ↓
  changes strategy
```

A chain may also emit higher-level pattern observations about its own lifecycle.

## Web UI

Web consumes semantic projection updates instead of recreating TUI grouping logic. Payloads carry neutral semantic state, not ratatui formatting.

## Telegram / channels

A long-running projection maps naturally to editable progress messages:

```text
same ProjectionId
revision 1 → send
revision 2 → edit
revision 3 → edit
terminal → final edit
```

Channel-specific rate limits remain in the adapter.

## DoD

DoD may correlate OS observations with Harw pattern instances, but a pattern match is never authority for privileged action.

Example drift:

```text
Harw projection says READY
DoD observes surviving process
→ drift finding
```

## Dream / Diary / Palace

Higher-level recurring patterns may become candidates for durable knowledge through explicit promotion/proposal paths.

Do not automatically persist every transient activity pattern into memory.

## Metrics

PatternInstances can produce derived metrics such as:

- compile_fix_cycles_total
- repeated_agent_failure_bursts_total
- file_read_burst_size
- job_lifecycle_duration

## Debugging

Potential diagnostics:

```text
/patterns active
/patterns explain <projection-id>
/patterns trace <pattern-instance-id>
```

Explain output should include definition/version, grouping key, constituent event IDs, reducer state, relation edges and closure reason.

## Export

Session/job exports may offer both raw and semantic layers. Semantic export never silently replaces raw audit data.

IMPLEMENTATION STATUS: planning only.
