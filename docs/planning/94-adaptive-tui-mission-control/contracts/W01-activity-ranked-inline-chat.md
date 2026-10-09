---
id: PL94-W01-ACTIVITY-RANKED-INLINE-CHAT
title: "W01 — Activity-ranked live messages inside the main Ratatui chat"
status: proposed
date: 2026-10-08
parent: ../README.md
---

# W01 — Dynamic inline live chat messages

> DESIGN ONLY, NOT IMPLEMENTED. Repository evidence from mm9942/Harwness dev (2026-10-08). PL-94 PR #137 is a separate planning workstream from Harw's implementation PRs #135/#136.

## Operator intent

The MAIN CHAT stays large in every orientation. A root orchestrator keeps running independently in the background, while its own *live, attached message* is visible within the chat. This live agent item can move between the latest messages according to foreground UIA activity, without changing real transcript order.

When multiple background agents exist, Harw should choose their visual priority based on observed, relevant activity — **not** on spawn order. This priority applies exclusively to the display. It must never alter execution/job priority, leases, rights, tool admission, permissions, budgets, approval flow, or agent contexts.

## CURRENT source-grounded baseline

- harw-tui/src/app/background_agents.rs: BackgroundLauncher::wants_background already defaults TUI-root orchestrator handoffs to background unless background:false; worker handoffs remain synchronous.
- harw-tui/src/child_stream.rs: ChildStreamBlock is an Arc/Mutex-backed live, bounded, recursively nested visible output block; ChildStreamRegistry keys it by child SessionId.
- harw-tui/src/app/child_stream_glue.rs: the child block is currently appended directly after ChildSpawned in canonical history cells, and *stays there*. It does NOT automatically move among the newest UIA messages.
- harw-tui/src/agent_monitor.rs: AgentLive and AgentMonitor track phases Admitted, Thinking, Tool, Waiting, Done, Failed and Cancelled, tool events, token usage, task, parent, visible output tail, failure, etc. This is the right source for a lightweight activity projection, not proof that activity ranking exists.
- harw-core/src/agent_events.rs: AgentEventHub broadcasts agent/session/parent-bound turn and orchestration observations but is lossy when consumers lag. Reconcile as needed from trusted active job/agent state; never make authorization or verified work claims based only on this bus.
- harw-tui/src/app.rs: push_line / push_cell append historical cells. harw-tui/src/chat_scroll.rs follows tail and preserves offset using total rendered line counts, not semantic cell identities. harw-tui/src/app/final_reply.rs separately prevents duplicate final answers.

## 1. Dynamic slots, numbered upward from Composer

Slots are VISUAL positions in the last part of the main chat. They are not durable transcript indexes. A background-agent item uses one stable display identity across event updates and repositions. It is never physically deleted and re-appended in the stored conversation.

| UI state | Closest to composer (#1) | #2 | #3 | Additional |
| --- | --- | --- | --- | --- |
| UIA idle, latest answer visible | Last committed UIA answer | Highest-ranked live background orchestrator | Next live background agent | Older historical cells |
| New user message submitted | New user message | Previous UIA answer | Highest-ranked live background orchestrator | Other background items / history |
| UIA answer in flight | User/foreground UIA response area stays in front | Foreground answer has its provisional display position | Highest-ranked background orchestrator | Other background items |
| UIA reply committed | New UIA answer | Highest-ranked live background orchestrator | Other active background messages / older history | Historical scrollback |

The exact temporary row ordering of user text and a not-yet-committed UIA response must be resolved for readable rendering; crucial invariants are: user/foreground UIA activity always comes ahead of all background live items, the top background item resides behind the foreground pair during the active turn, and returns directly behind the final UIA answer afterwards. Do not turn temporary placement into a permanent change to real chronological chat history.

With one root orchestrator, this produces the operator's usual #2 idle / #3 while a new UIA turn is underway / #2 again when finished behavior. With multiple running agents, the most active relevant candidate occupies the lead background slot; the other background live items follow, ordered by activity. The root is not artificially forced to rank first, but an explicit user pin can keep it selected.

## 2. Priority based on real activity, with stability

Use a deterministic, pure, session-authorized LiveActivityRanker in the TUI projection:

1. **Needs operator attention**: explicit pending question/approval or newly observed critical failure. Approved modal dialogs keep absolute priority and are not replaced by chat rank.
2. **Meaningful progress**: tool completion with outcome, new verified milestone, bounded child result, task/phase transition, fresh authorized assistant message.
3. **Active work**: tool-in-progress or currently thinking with recent meaningful event.
4. **Waiting or quiet**: still running, but no recent meaningful observation. Quiet means UNKNOWN/WAITING, not completed.
5. **Terminal**: completed/failed/cancelled leaves the live stack after a bounded final display; retains discoverable history and Finished/Failed status.

Ranking is scoped to the selected UIA session and permitted descendant runs; filter identity/visibility BEFORE scoring. A model's text saying "urgent" has zero authority to boost its own rank. High token throughput, repeated reasoning deltas or spinner frames do NOT equate to meaningful activity. Favor trusted event phases/outcomes and admitted task links to current user intent.

Recency decays; ties are deterministic (previous rank, then stable run id). Use a short event coalescing interval and minimum visible dwell, with a sufficient score margin for ordinary switches, to prevent visual bouncing on every token. Initial tuning suggestion: coalesce 0.5–1 s, keep the current lead for roughly 3 s unless a real urgent operator/approval/failure event occurs. These numbers are proposed UX parameters, not current system values.

A manually selected/pinned agent overrides automatic presentation ranking until explicitly unpinned or terminal. The whole agent tree remains accessible, including nested child runs; do not expose private child contexts. At terminal widths where more than one live block would crowd out Chat, expand ONE top-ranked block and show remaining candidates as tiny independently selectable status rows or a "+N weitere aktiv" indicator.

## 3. No context mixing, execution changes or history reordering

- The live background item is a read-only virtual HistoryCell/ChildStreamBlock reference keyed by trusted session/run and a UI-only stable cell identity; it is not a newly committed User or Assistant message.
- Canonical user/assistant message history remains append-only and chronologically true. Export, replay, model context, memory indexing and session persistence must not reflect virtual live-slot movement or duplicate messages.
- All visible content comes through the existing scope checks and sanitized bounded projections. Never leak private reasoning, full child messages, tool args or hidden transcripts from other agents merely because they occupy neighboring UI positions.
- Child activity does not make the UIA ingest the child's whole session. Only bounded, authorized result/progress envelopes enter the parent.
- When manually scrolled away from the end, freeze automatic reordering and preserve an identity-based top-visible-cell anchor. Show "N neue Aktivitäten" until the operator returns to tail. The existing ChatScroll line-count-based sync alone is insufficient for reordered virtual segments.
- Foreground user input, typing, approvals, scrollback and current turn must not be interrupted by rank changes. Rotation and reorder are display-only; the background jobs keep their own leases/identity/budgets and continue without respawn.
- EventHub broadcast may lag. Reconciliation must never resurrect a terminal child or mark an unverified action Completed.

## 4. Implementation ownership and waves (proposed)

| Wave | Owner | Change |
| --- | --- | --- |
| M2a | harw-tui app / history rendering / child_stream_glue | Stable per-child live virtual segment and foreground slot projection; no reorder of canonical cells. |
| M2b | harw-tui agent_monitor adapter / pure activity ranker | Activity/recency weighting, manual pin, anti-thrash and session visibility filtering. |
| M2c | harw-tui chat_scroll and renderer | Semantic cell anchoring, tail slot transitions, bounded live rendering and Ctrl+O/detail preservation. |
| M3+ | Existing Plan/Job/Progress projections | Let verified progress affect presentation only; do not give progress agent an execution-priority API. |

No new runtime scheduler or agent-context pool. Preserve background-by-default and do not write into Harw-owned PL-90 code files concurrently.

## 5. Acceptance tests, all currently NOT RUN

1. One background root: idle #2, new user message moves it to #3, current UIA reply retains foreground, final answer restores root #2. No run restart or identity change.
2. Three distinct background orchestrators: meaningful recent task activity can promote a different run into lead; older root remains reachable; no effect on actual job execution order.
3. 500 streamed tokens/reasoning deltas from a chatty agent cannot steal lead from one with a verified milestone or pending question.
4. Foreground user/assistant pair remains legible and never loses an input, final answer, correction or export message despite live rearrangement.
5. Scroll-up freezes auto reordering and preserves the exact same top-visible cell; follow-bottom resumes rank updates.
6. User pin keeps the selected root visible until release; another agent's urgent approval still appears as a dialog.
7. A completed/failed run exits live ranking without losing its failure reason or final result in history/status.
8. Each live visual handle is bound to its own authorized SessionId/run; no child transcript mixing across concurrent agents, roles or tenants.
9. Bus lag, stale events, repaint, terminal rotation or resume do not resurrect jobs, duplicate cells or alter permissions.
10. The main chat dominates available space on portrait and landscape, with compact background candidates rather than a second giant window.

Definition of done: background agents run separately and can be inspected inside the main chat; their visible *live* messages reposition predictably by observed activity around new user/UIA replies, without modifying the canonical transcript, execution priority, context or approvals.
