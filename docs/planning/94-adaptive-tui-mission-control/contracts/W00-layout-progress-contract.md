---
id: W00-PL94-ADAPTIVE-MISSION-CONTROL
title: "W00 Contract — three status cards, responsive mission rail and Progress Agent"
status: proposed
date: 2026-10-08
parent: ../README.md
---

# W00 — Adaptive Mission Control acceptance contract

> **DESIGN ONLY**, rooted in dev@197a92e92d438bf6bf93b129bb964f4852686149. See the [parent design](../README.md). No renderer, agent or new runtime listener is claimed as implemented.

## 1. Exact UI state sources and authority

| View | Read source | What it may write |
| --- | --- | --- |
| Active agents | Existing AgentMonitor, AgentEventHub projection and persisted child status where needed | UI focus/selection only |
| Active jobs | Existing JobManager JobStatus/JobRow and events | UI focus/selection only |
| Finished / failed | Finished/failed projections of those same agents/jobs, with verified lifecycle and failure reason | Mark UI acknowledgement; do not alter job outcome |
| Work in Progress | Authenticated session-specific last three committed user messages, current main turn's *public* observable state, PlanStore/GoalStore status and linked job/agent progress | Sanitized display; optionally bounded draft Todo **proposals** to the existing plan admission flow |
| Progress Agent | Isolated, least-privilege observer, optional model-generated compact progress narrative | New proposal document/turn-bounded output only; never parent/child history, authority or executable plan mutation |

**Status must never be inferred from free-form assistant output:** a claim that a test passed is still a claim until supported by an actual verified job/evidence result. WorkDriver/Goal owner acceptance is not delegated to the Progress Agent.

## 2. Responsive rect contract

- Compute from terminal cells \`x=columns, y=rows\`, not OS or model. \`x>y\` prefers landscape; \`y>x\` prefers portrait; \`x==y\` has a deterministic feasibility tie breaker.
- A side rail is only feasible if \`chat.width >= 68\`, \`rail.width >= 44\` and sufficient body height remains after footer; an otherwise landscape-oriented terminal falls back to **ShallowLandscape**. Never call portrait purely because it is a phone.
- In **PortraitStack**, Agents and Jobs sit in the pinned first row (two logical cards), **Finished/Failed** immediately underneath, then a taller separate **WIP** panel and the main chat. Composer/status remain at bottom.
- In **LandscapeRail**, chat is left, right rail holds Agents/Jobs at top, Finished/Failed underneath, and WIP in the large remaining right region. Independently scrollable.
- In constrained \`Summary\`/compact mode, label all 3 categories as distinct counters with a detail switcher and show the latest verified progress in one or two lines. Do not construct unreadable zero-height cards.
- Minimum chat, modal approvals and footer win capacity competition. Compress WIP first, status detail second; do not clip approval prompts.
- Pure classifier; saturating arithmetic; no overlaps, out-of-bounds rectangles or unchecked conversion. A layout flip on resize does not modify agent/job execution, cancellation, history, plan or user input.
- Preserve current F2/F3/F4/F5/F11/Esc semantics; WIP focus must not steal keystrokes from composer. Hit test by rectangles, not assumptions that AgentMonitor is always on the right.

## 3. Observer input/output contract

Trusted input projection only:

    ProgressInputV1 {
      principal: runtime-bound tenant/workspace/session/run,
      user_messages: last 0..3 committed user-authored messages IN THIS session,
      current_turn: {turn_id, user_intent_revision, phase, current_public_task, safe_output_tail?},
      progress: {plan_revision?, nodes?, agent_statuses?, job_statuses?, evidence_refs?, blockers?},
      watermark: committed source event sequence,
      input_digest: canonical hash over admitted projection
    }

Model output is an **untrusted proposal**:

    ProgressProposalV1 {
      source_session, source_turn, source_watermark, input_digest,
      narrative, next_step,
      todo_drafts: [{title, kind?, dependency_refs?, evidence_refs?}],
      proposed_status_changes: [{plan_node_ref, status, evidence_refs, reason}],
      uncertainties, optional_citations
    }

**Hard authorization:** input principal is set by runtime; a model cannot choose the session, project, agent, worktree, source index, tool grants or read scope. A child/sibling result appears only through its already admissible bounded return envelope. Never include private reasoning, raw prompts, raw tool args, unrelated user messages or secret material. Model narration and proposals are escaped/redacted, output capped and are never executable commands.

**Write and accuracy rules:** Progress Agent is read-only and tool-free in the MVP. The deterministic coordinator may hold session-local draft cards but may not mark a PlanNode Completed or Goal Achieved. Actual persisted changes require the existing validated PlanStore actions and approvals. \`Completed\` carries verified EvidenceRef; \`Failed\` and \`Blocked\` are never disguised as “almost done”.

**Lifecycle:** event-triggered coalesced updates after trusted commits, plan changes, job progress or child return. Do not trigger an LLM on every rendered frame, output token, tool heartbeat or polling interval. One bounded pending model update/session; reject stale output after new user correction, plan revision or session move. If no provider, render deterministic status with explicit deferred/stale labels. Preserve a cursor/high-water mark for restart; lossy AgentEventHub notifications are only wakeups.

## 4. First-pass test gates

### Layout
1. Geometry at \`48x65\`, \`70x90\`, \`80x40\`, \`100x30\`, \`120x42\`, \`160x45\`, \`39x60\`, \`48x19\`, \`1x1\` and \`0x0\`; x/y tie and crossing on resize.
2. Rectangles non-overlapping and within viewport; status/composer modal reserve, focus, F2/F3/F4/F5/F11/Esc, Unicode width, modal priority and independent wheel routing unchanged.
3. Three distinct status views share source truth with existing monitor and JobRows; no duplicate lifecycle; finished/failed reason visible.
4. No unbounded rendering or excessive main-chat area loss.

### Progress
5. Exactly three newest committed **user** messages or fewer; no sibling/other session content; latest correction supersedes stale output.
6. Untrusted model output cannot mutate PlanStore, JobManager, active agent turns, permissions or Goal achieved state.
7. Job/agent failure remains visible even when a model writes a successful-sounding summary; verified evidence wins.
8. Agent failure, provider outage or disabled feature leaves deterministic WIP visible, not an infinite spinner.
9. Same session resume restores cursor and chosen plan; switching session cannot reuse stale observer output.
10. Multiple concurrent sessions keep separate observer states, jobs, budgets and output; no hidden reasoning leakage.
11. Runtime output/text remains capped and redacted; all user/agent/tool content treated as lower-trust data.
12. No new hot-path full-history scan, unconditional model call or accidental restart of main worker on resize.

## 5. Wave owner boundaries

- M1: \`harw-tui-layout\` pure classifier, geometry tests.
- M2: \`harw-tui\` status cards, focus/scroll, renderer tests.
- M3: runtime-to-TUI typed progress projection without an LLM.
- M4: optional separated progress agent job with strict minimal context.
- M5: validator for model todo drafts using existing PlanStore; retain human-only acceptance.
- M6: reconcile PL-90 session index and job progress without sharing agent context.

This W00 should not be used to justify touching Harw-owned code in PR #136 or moving runtime policy into \`harw-tui-layout\`. Record actual implementation SHAs and tests before any item is relabeled COMPLETE.