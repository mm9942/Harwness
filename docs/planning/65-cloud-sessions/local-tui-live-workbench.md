---
id: PL-65-LOCAL-TUI-WORKBENCH
title: "Local TUI — real-time multi-session agent workbench"
status: proposed
date: 2026-10-01
tags: [planning, tui, sessions, agents, realtime, cloud]
related:
  - README.md
  - "https://github.com/mm9942/Harwness/blob/coop/claude-doc-patches/docs/planning/65-cloud-sessions/CLOUD-HOME-HUB-PROFILES-V2.md#8-first-party-attached-tui"
  - local-own-cloud-websocket.md
  - ../../68-mobile-tui/README.md
  - ../../design/tui-architecture.md
---

# Local TUI — real-time multi-session agent workbench

> **Planning document.** This document describes the requested TUI behavior and
> implementation boundaries. It does not claim that the behavior is implemented.
> The related Harw plan store tracks architecture (`h6`), implementation (`h7`)
> and one consolidated verification pass (`h5`).

## 1. Purpose

Harw is being built as a cloud AI workbench for a user who thinks, explores,
plans and implements in real time across multiple sessions and agents. The TUI
must let the user continue an ordinary conversation while following the work
of multiple Roots and their agents. A running orchestration must not take over
or disable the chat composer.

This is a **client/workbench interaction plan**. It complements PL-65's remote
session hosting and transport work. The Cloud Home Hub v2 proposal
([`CLOUD-HOME-HUB-PROFILES-V2.md`](CLOUD-HOME-HUB-PROFILES-V2.md), §8)
identifies the first-party TUI as a client/composition gap: an attached TUI
should use `SessionPort`, keep only presentation state locally, send typed
intents, and must not own a `RuntimeAssembly` or become a second session host.

This plan specifies the live multi-session interaction and display semantics
that should be shared by the embedded/offline and future attached TUI paths. It
does not itself introduce a new cloud backend, protocol, daemon, session-host
architecture, or claim that the attached TUI is already implemented.

## 2. Current implementation baseline

The read-only architecture review for the associated Harw plan found:

- `ChatApp` is currently bound to one session. `HarwEvent::Submit`,
  `SystemMessage` and `Command` carry text but no session/root destination.
- The TUI has separate transcript and editor state. The scroll model is
  tail-relative and already supports follow mode and preserving the user's
  offset while scrolled back.
- Root/agent events are available through `AgentEventHub`; agent events carry
  stable session IDs and optional parent links, and orchestration events expose
  root/parent/child relationships.
- Durable session transcripts are append-only. The new display projections
  must not rewrite history or change persistent message/event IDs.
- Existing tests cover portions of scroll, event and session wiring; explicit
  tests for the requested recency slots and session-safe submit routing were
  not identified in the read-only review.

These observations are a starting point for implementation, not a claim that
all routing or multi-session behavior already exists.

## 3. Required behavior and invariants

### 3.1 Chat remains live

- Keep the composer available while Roots and agents run.
- Append every newly submitted user message at the bottom of the selected
  session's chronological chat.
- Show the destination session/root before submission.
- Capture the selected `SessionId` at submit time; never infer or silently
  change the destination later because the visible session changed.
- If the selected session is busy, apply that session's explicit FIFO/follow-up
  semantics. A message must not be delivered to another session.

### 3.2 Follow multiple Roots and agents

- Display live Root and agent activity for multiple sessions with unambiguous
  session and parent/child association.
- Keep persistent identifiers stable. The UI may project events into recency
  positions, but must never renumber stored messages or agent sessions.
- The newest agent response occupies display slot 1. The former slot 1 moves to
  the linked slot 2. Older responses remain in chronological scrollback.
- Scrolling the transcript must not lose, rewrite or re-order the durable
  transcript; live-follow and manual scrollback remain distinct modes.

### 3.3 Transient status notices

- Temporary agent/system status notices (including failures and budget notices)
  remain prominent in the active view for at most 30 seconds.
- After that interval, a TOML setting selects either per-session compact
  consolidation or hiding from the active view (`consolidate` or `hide`). The
  proposed default is `consolidate`.
- Consolidation/hiding is presentation-only. It must not delete the durable
  transcript or event/audit evidence.
- Age-based behavior must be deterministic and covered for both TOML modes.

### 3.4 Goal-follow mode

- Provide a toggleable, read-only TUI projection of the active goal and plan state
  for the currently selected session. The projection is presentation state; it
  does not become another source of truth or alter the stored plan.
- Show only evidence-backed progress, explicitly list open criteria and blockers,
  and identify the next executable plan node. Do not infer completion from
  activity or mark any criterion or node done without supporting evidence.
- Refresh the projection as goal/plan state changes. Clearly indicate when the
  selected session has no valid binding to an active goal/plan, or when the
  available binding/state is stale; do not present missing or stale data as live.
- Toggling or viewing this mode must leave the chat composer and transcript
  scrollback available and unaffected.
- This mode is strictly observational: it must never change permissions,
  approval requirements/decisions, or step status.

## 4. Work packages

1. **Architecture and message targeting:** retain composer and transcript as
   distinct surfaces; define session selection, submit-time target capture,
   active-turn queue behavior and visible target indication.
2. **Live multi-session projection:** correlate Root/agent events by stable
   session and parent IDs; maintain the two recency references as a UI
   projection; preserve complete chronological scrollback.
3. **Transient status policy:** implement the 30-second prominence window and
   the TOML `consolidate`/`hide` modes without deleting durable records.
4. **Consolidated verification:** add deterministic tests for routing,
   interleaved sessions, composer availability, recency-slot movement,
   scrollback, expiry and both TOML modes. Run the affected test/build checks
   together after the implementation work is complete, not after each slice.

## 5. Acceptance checks

- With a Root active in session A, the user can keep typing and submit a message
  explicitly to A or another selected session B. Switching the visible session
  after submission cannot change the captured destination.
- Interleaved Root/agent events from A and B remain attached to the correct
  session and parent; a session switch does not stop or mix either run.
- Each new agent response moves into slot 1; the previous slot 1 becomes the
  linked slot 2; subsequent older responses remain available through scrollback.
- A transient status is prominent before 30 seconds and follows the selected
  TOML mode after expiry. The underlying transcript/event record and stable IDs
  are unchanged in both modes.
- Existing tail-follow, manual scroll offset, approval/modal precedence and
  session isolation remain intact.
- Goal-follow mode can be toggled for the selected session and displays the
  active goal/plan projection with evidence-backed progress, open criteria,
  blockers and the next executable node; missing or stale session binding/state
  is clearly identified, and live updates do not disrupt composer or scrollback.
- Goal-follow mode is read-only: it changes no permissions, approvals or step
  statuses, and does not mark unsupported work complete.

## 6. Boundaries and review notes

- No broad Cloudflare/remote-session backend or transport changes are part of
  this TUI work package.
- No changes to the already user-confirmed model steps h2–h4 are included here.
- The workspace had unrelated existing changes when this planning work began;
  the draft PR must list its included source/docs explicitly and must exclude
  secrets and generated build/report outputs.
- Tests and builds are intentionally deferred to the consolidated verification
  step. A Draft PR may be reviewed while verification is pending, but must say
  so clearly.
