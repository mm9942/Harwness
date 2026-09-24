# Knowledge & Work Surfaces

Status: design draft. No code exists yet for `harw-knowledge`, `harw-session-store`,
`harw-job-runtime`, `harw-catalog`, or `harw-policy` — this document specifies the
shape they must have before implementation starts.

> **Ist-Stand (2026-09-24, Runde 4).** The "no code exists yet" line above is
> historical. `harw-knowledge`, `harw-session-store`, `harw-job-runtime` and
> `harw-catalog` exist; store, index, recall and visibility are implemented,
> and `ArtifactKind` has more variants than the 8 sketched in §8.1. All five
> surfaces are wired as operator commands in `harw-ops` and registered in
> `register_all` (grammar: `interaction-contract.md` §2.2):
>
> - **Common base (D0):** one shared argument parser
>   (`harw-ops/src/knowledge_args.rs`); cross-process file locks
>   (`harw-knowledge/src/lock.rs`, `fs4` advisory `flock`/`LockFileEx` on a
>   hidden `.<name>.lock` neighbour, with timeout) for diary appends, kanban
>   cards/boards, workbench manifests and palace nodes; structured side files
>   (diary `.jsonl`, dream report data) instead of re-parsing Markdown; reads
>   use the real caller's principal (operator → `OperatorOnly`, agent →
>   `SelfOnly`, fail-closed); live updates via
>   `AgentEventKind::Knowledge { area, id }` on the `AgentEventHub`.
> - **Workbench (D1):** note edit/remove, per-scope retention, read tool
>   `workbench.show` and a context provider for pins and hypotheses.
> - **Kanban (D2):** worker lanes are served by the job worker, but a card
>   never starts an agent without `/kanban approve`; risk per role; edit,
>   comments, evidence, history and result on the card; read tools
>   `kanban.list`/`kanban.show`.
> - **Diary (D3):** automatic `compaction` and `end-of-session` entries
>   (`harw-runtime/src/diary_wiring.rs`), range view and search, read tool
>   `diary.read` (own entries only), retention `[knowledge.diary]
>   retention_days` (`config-scopes.md` §1.17).
> - **Palace (D4):** bridge `/memory promote` (fact → `provisional` topic),
>   `/palace supersede|edit|link` behind an operator review gate, an
>   mtime-invalidated index cache, read tools `palace.search`/`palace.recall`
>   (`established` only, bounded hops).
> - **Dream (D5):** `/dream run|status|review`, structured JSON output with
>   one repair turn, a `JobKind::Dream` ledger job, `[dream]` configuration
>   (`config-scopes.md` §1.18), knowledge maintenance on every run
>   (diary rollup/gc, workbench retention, palace staleness *suggestions*).
>
> Still open: `harw-policy` does not exist as a crate, so cross-tree
> visibility enforcement (§7) remains a gap; child sessions have no diary
> recorder. A dream mirror card on the kanban board was deliberately dropped
> (Kanban only on the user's explicit request).

## 0. Why one document

Harwness needs six things that look unrelated but share one storage and
governance substrate: a place agents keep durable facts, a place they keep a
daily log, a place idle time gets spent productively, a place a session keeps
its current working set, and a place multi-agent work items get tracked and
routed to workers. Building these as five ad-hoc stores would mean five audit
paths, five visibility models, and five serialization formats. Harwness builds
them as **one coherent subsystem**: a single markdown+frontmatter store, one
typed index, one governance surface (visibility scopes, budgets, leases), and
one crate family that different front-ends (`/memory`, `/diary`, `/dream`,
`/palace`, `/workbench`, `/kanban`) render views over.

Inspiration is drawn from OpenClaw's memory/dreaming/wiki plugins and Hermes's
Kanban worker lanes, studied for their *problem shape*, not copied for their
mechanism. Concretely, Harwness diverges from both in three ways worth naming
up front:

1. Dreaming is not opportunistic background magic bolted onto a chat runtime —
   it is a normal `harw-job-runtime` job with a budget, a lease, and an
   operator-visible audit trail, subject to the same governance as any other
   agent work.
2. The palace is not an embedding index; it is a markdown graph of
   `[[wikilinks]]` with a keyword/tag/link-based bounded retrieval interface —
   deterministic, diffable, offline-auditable, no vector store dependency in
   the core crate.
3. Kanban and memory are the same storage family, not two products. A card and
   a diary entry are both `KnowledgeArtifact`s with different `ArtifactKind`s,
   indexed by the same `KnowledgeIndex`, gated by the same visibility scopes.

## 1. Unified storage concept

### 1.1 Crate shape

One crate, internally modularised, rather than five crates that would need to
agree on a wire format via duplication:

```
harw-knowledge/
  src/
    lib.rs           //! crate root, re-exports
    error.rs         // KnowledgeError (via harw-macros::HarwError)
    store.rs         // filesystem layout, atomic write, frontmatter parse
    index.rs         // KnowledgeIndex: typed, in-memory, rebuildable from disk
    artifact.rs       // KnowledgeArtifact, ArtifactKind, Frontmatter
    memory/
      mod.rs
      core.rs        // core memory file (single durable summary)
      topic.rs       // topic memory files
      palace.rs      // palace graph: nodes, [[wikilinks]], backlinks
      recall.rs      // bounded retrieval interface
    diary.rs         // append-only daily journal
    dream.rs         // dream job payload/output types (job itself lives in harw-job-runtime)
    workbench.rs      // per-session/per-project scratch surface
    kanban/
      mod.rs
      board.rs       // Board, Lane, Card
      lifecycle.rs   // card <-> WorkId state mapping
    visibility.rs    // VisibilityScope, applies uniformly to every surface
```

Splitting further (e.g. a separate `harw-kanban` crate) is deliberately
rejected for v1: kanban cards and palace nodes are both indexed, visibility
checked, and persisted through the same three functions
(`store::write_atomic`, `store::read_frontmatter`, `index::rebuild`). A split
crate would either duplicate that or create a circular dependency back into
`harw-knowledge`. Revisit only if kanban outgrows markdown storage (e.g. needs
transactional multi-writer semantics beyond what file locks give it).

### 1.2 On-disk layout

Everything lives under one workspace root, mirroring how `harw-session-store`
already roots session directories:

```
<workspace_root>/
  knowledge/
    core/MEMORY.md                     # layer 1: core memory
    topics/<slug>.md                   # layer 2: topic memories
    palace/<node-id>.md                # layer 3: palace nodes ([[wikilinks]])
    diary/<agent-id>/<YYYY-MM-DD>.md   # append-only daily journal
    dreams/<YYYY-MM-DD>/<job-id>.md    # dream job output (proposals, not facts)
    workbench/<scope-id>/              # session- or project-scoped scratch
      NOTES.md
      pinned/                          # pinned file references (symlink-free: path list)
      hypotheses.md
    kanban/
      boards/<board-id>/
        board.toml                     # Board metadata
        cards/<card-id>.md             # one card per file
    .index/
      knowledge.idx                    # rebuildable cache: id -> path, tags, links, scope
```

Each markdown file carries a YAML frontmatter block with a stable schema
(`id`, `kind`, `created_at`, `updated_at`, `visibility`, `tags`, `links`,
`author_agent_id`, plus kind-specific fields). The frontmatter is the
canonical structured data; markdown body is prose. This mirrors how
`harw-config` and `harw-protocol` already prefer explicit typed structs with
serde derives over implicit convention — the frontmatter schema is a Rust
struct (`Frontmatter`), not a loose map, though an `extra: BTreeMap<String,
serde_json::Value>` escape hatch exists for forward compatibility.

### 1.3 Typed index

`KnowledgeIndex` is an in-memory structure rebuilt from disk at startup (and
incrementally updated on writes), not a database. It answers four query
shapes without a full directory walk:

- by id (`get(id) -> Option<&KnowledgeArtifact>`)
- by tag/kind (`find(kind, tags) -> Vec<ArtifactRef>`)
- by link (`backlinks(node_id) -> Vec<ArtifactRef>`) — powers the palace graph
- by visibility-filtered scope (`visible_to(agent_id) -> impl Iterator<...>`)

Rebuild is intentionally cheap (markdown+frontmatter parse over a bounded
directory tree) so a corrupted or hand-edited index file is never a source of
truth loss — `harw-knowledge doctor` can always regenerate it from the
markdown files, which remain the durable record.

### 1.4 Relationship to `harw-session-store`

`harw-session-store` owns **transcripts**: the sequential, replayable record
of turns, tool calls, and model outputs for a `SessionId`/`ThreadId`. It is
append-only, high-volume, and largely uninteresting after a session ends
except for replay/audit.

`harw-knowledge` owns **artifacts**: curated, comparatively low-volume,
durable-by-design material meant to be read *by future sessions*, not just
replayed. The boundary is intentional and enforced structurally:

| | `harw-session-store` | `harw-knowledge` |
|---|---|---|
| Unit | Turn / ToolCall / Item | KnowledgeArtifact |
| Growth | Unbounded, append-only | Curated, promotion-gated |
| Read pattern | Sequential replay by session | Indexed lookup by tag/link/scope |
| Written by | The turn loop, automatically | Agents (explicitly), dream jobs (proposals), promotion (compaction) |
| Deleted? | Retention-policy governed, rarely by agents | Rarely; superseded via versioning, not silent overwrite |

The only sanctioned bridge between them is **promotion**: a session-store
transcript segment can be *distilled* into a diary entry or topic memory
(never copied wholesale), and a `KnowledgeArtifact` can carry a
`source_session_id: Option<SessionId>` provenance field pointing back at the
transcript it was distilled from, so a reader can always ask "where did this
belief come from."

## 2. Memory system

### 2.1 Three layers

1. **Core memory** — one file, `knowledge/core/MEMORY.md`. Durable facts,
   standing preferences, decisions. Small by design: loaded in full at the
   start of every session for the owning agent. Growth pressure here is a
   signal to demote detail into topic memory, not to let the file grow.
2. **Topic memory** — `knowledge/topics/<slug>.md`, one file per coherent
   subject (a project, a person, a recurring task class). Detailed, not
   bootstrap-loaded; retrieved on demand via the recall interface.
3. **Per-session recall** — nothing durable of its own; it's the runtime
   behavior of a session pulling topic/palace material into its working
   context via bounded search (§2.3), scoped to what that session's
   `VisibilityScope` allows.

Promotion flows strictly downward in freshness, upward in durability:
session recall observations -> diary entries -> topic memory -> (rarely) core
memory; and topic memory -> palace nodes when a piece of knowledge earns a
permanent, linkable place (§2.2, §2.4 covers the exact gate).

### 2.2 The palace: a linked long-term memory graph

The palace is the structured tier above topic memory: a directed graph of
markdown nodes under `knowledge/palace/`, connected by `[[wikilink]]`
references in the body text, indexed via `backlinks()`. Each node is one
`KnowledgeArtifact` of kind `PalaceNode` with:

- a stable `id` (slug-derived, human-legible, e.g. `palace/deploy-pipeline`)
- a title and prose body containing `[[other-node]]` links
- `tags: Vec<String>` for topical clustering independent of the link graph
- `confidence: Confidence` (Established | Provisional | Superseded) — nodes
  are never silently deleted; a superseded node stays with a `superseded_by`
  link so history is traceable

Unlike topic memory (one file per subject, flat), the palace is explicitly a
*graph* — the point is that traversing links surfaces adjacent knowledge a
keyword search alone would miss ("the deploy pipeline node links to the
on-call rotation node, which links to the escalation contact node"). This is
the one deliberate structural difference from OpenClaw's memory-wiki, whose
compiled wiki vault is closer to a curated document set than a
link-traversable graph — Harwness makes the graph itself part of the query
surface, not just a rendering nicety.

### 2.3 Retrieval interface: bounded search

`recall::search(query: &RecallQuery) -> RecallResult` is the single read path
every surface uses (session bootstrap, `/memory recall`, dream jobs, kanban
card context). It is bounded on three axes so it can never turn into an
unbounded context dump:

- `max_artifacts: usize` — hard cap on results returned
- `max_hops: u8` — how far to traverse palace backlinks from a keyword/tag hit
- `visibility: VisibilityScope` — caller's scope; artifacts outside it are
  invisible, not just filtered client-side

Matching is keyword + tag + link-graph traversal (BM25-style scoring over
frontmatter tags and body text, no embedding dependency in the core crate).
An embedding-backed ranking layer is left as an explicit extension point
(`RecallRanker` trait) so a future `harw-knowledge-embeddings` add-on can
plug in without changing the interface every surface already depends on —
deliberately deferred rather than baked in at v1, matching the "no unproven
dependency" bias in this codebase's Cargo hygiene.

### 2.4 Write policy: agent-writable with audit

Agents write memory directly — there is no human-in-the-loop gate on an
ordinary `memory_write` call, because requiring approval for every note would
make memory unusable. What *is* enforced:

- every write produces an **audit record**: `author_agent_id`, `written_at`,
  `source_session_id`, and (for edits) the previous version's id, appended to
  a per-artifact history rather than overwriting in place
- writes to **core memory** and **palace promotion** require the writing
  agent's `VisibilityScope` to include `OperatorOnly`-adjacent write rights
  *or* pass through the compaction/dream promotion path (§2.5) — a plain
  session agent can propose a core-memory change but the promotion path is
  what actually commits it, keeping the bootstrap-loaded file small and
  reviewed
- topic memory and diary are freely agent-writable within the writer's own
  scope; nothing above OperatorOnly review is required to write there, only
  to *promote out of* them

### 2.5 Compaction / promotion rules

Promotion is a pipeline, not a single step, matching the layered model in
§2.1:

```
session turn -> (compaction trigger) -> diary entry (raw-ish, dated)
diary entries -> (dream job, scored) -> topic memory candidate
topic memory  -> (dream job or explicit /palace promote, scored + reviewed) -> palace node
```

Each arrow is a distinct, named operation with its own gate:

- **compaction -> diary**: triggered automatically before a session's
  transcript is summarized/dropped (mirrors the "memory flush before
  compaction" idea, generalized to "flush to diary," not directly to
  `MEMORY.md` — Harwness never lets an automatic hook write core memory).
- **diary -> topic**: a dream job (§4) scores diary entries by recurrence,
  recency, and tag overlap; candidates crossing a threshold become topic
  memory entries or amend an existing one. This is a *proposal* the dream job
  writes to its own output file; a separate, lightweight commit step (can be
  automatic if `dream.auto_commit_topic: true`, default false) actually
  writes to `knowledge/topics/`.
- **topic -> palace**: always requires either an explicit `/palace promote
  <topic-id>` from an agent/operator, or a dream job proposal that a human
  reviews via `/dream review`. This is the strictest gate because palace
  nodes are treated as durable truth other agents will traverse and trust.

## 3. Diary

### 3.1 Schema

One file per agent identity per day: `knowledge/diary/<agent-id>/<YYYY-MM-DD>.md`.
Append-only within a day — entries are never edited in place, only appended,
each with its own frontmatter-less inline header (`### HH:MM:SS — <trigger>`)
followed by prose. The file-level frontmatter (once, at top) carries
`agent_id`, `date`, `visibility`, and an `entry_count` maintained by the
writer.

### 3.2 Triggers

- **End of session** — a session's final turn, if it produced anything worth
  recording (heuristic: non-trivial tool use, a decision, an error resolved),
  appends one entry summarizing the session.
- **Compaction** — the pre-compaction flush (§2.5) writes a diary entry
  capturing what would otherwise be lost when the transcript is summarized.
- **Idle-time reflection** — a dream job (§4) may append a reflective entry
  when it notices a pattern across several diary entries, tagged distinctly
  (`### HH:MM:SS — dream-reflection`) so a reader can tell agent-authored
  entries from dream-authored ones at a glance.

### 3.3 Retention

Diary is the working layer, not the archive layer — retention is bounded by
default (`diary.retention_days`, default 90) after which entries older than
the window are eligible for `harw-knowledge gc` to compact into a single
monthly rollup file (`diary/<agent-id>/rollup/<YYYY-MM>.md`) rather than
deleted outright, so nothing durable is lost, but the daily-file directory
doesn't grow unbounded. Rollup itself is a summarization step subject to the
same promotion audit trail as any other write.

## 4. Dream

### 4.1 What it is

A **dream job** is ordinary governed work: a `harw-job-runtime::Job` with a
`Budget` (token/time/tool-call ceiling), a `Lease`, and retry semantics —
scheduled during idle time (no active session claiming an agent's turn
budget) rather than a special always-on background thread bypassing
governance. This is the single largest deliberate departure from OpenClaw's
model: dreaming there is a plugin-owned cron sweep outside the normal agent
work-tracking path; here it is *the same primitive* as any other job a
`kanban` lane worker would run, just self-scheduled and narrowly scoped.

### 4.2 Guardrails

A dream job runs with a fixed, minimal capability set enforced by
`harw-policy`, not by convention:

- **No tool escalation** — a dream job's `ToolPolicy` is a strict subset of
  its owning agent's normal policy: read access to diary/topic/palace under
  its own visibility scope, write access only to its own `dreams/<date>/`
  output file. It cannot invoke arbitrary tools, cannot reach the network,
  cannot write core memory or palace nodes directly.
- **Output is proposals/artifacts, not committed truth** — a dream job
  produces a `DreamReport` (consolidation summary, proposed topic-memory
  edits, proposed palace promotions, surfaced follow-ups) written to
  `knowledge/dreams/<date>/<job-id>.md`. Nothing in `MEMORY.md`,
  `topics/*.md`, or `palace/*.md` is touched except through the explicit
  commit step described in §2.5, which defaults to requiring review.
- **Operator-visible** — every dream job run is a normal `WorkId` in
  `harw-job-runtime`'s ledger: visible in `/kanban` as a system-lane card (or
  in a dedicated `/dream status` view), with the same lease/heartbeat/timeout
  machinery as any worker card, so a stuck or runaway dream job is caught by
  the exact same stranded-work detection as a stuck coding task, not a
  separate ad-hoc watchdog.

### 4.3 Budgets

Dream jobs are configured with a conservative default budget
(`dream.budget.max_tokens`, `dream.budget.max_wall_seconds`,
`dream.budget.max_tool_calls` — the last near-zero, since dream jobs mostly
read+summarize) and a schedule (`dream.schedule: cron-expr`, disabled by
default, matching the opt-in posture memory-adjacent background work should
have).

## 5. Workbench

### 5.1 What it holds

The workbench is a persistent scratch/working-set surface scoped to either a
session (`workbench/<session-id>/`) or a project
(`workbench/project:<project-slug>/`) — project-scoped when several sessions
collaborate on the same longer-lived effort and shouldn't lose context on
session boundary. It holds three kinds of content, each a distinct file so
the TUI can render them as distinct panels:

- **Pinned files** — `pinned/` is a flat list of absolute paths (not copies,
  not symlinks) the working session cares about right now; a manifest file
  (`pinned/MANIFEST.md`) lists them with one-line annotations of *why* each
  is pinned.
- **Notes** — `NOTES.md`, free-form scratch prose, the lowest-ceremony
  surface in the whole subsystem (no promotion gate, no frontmatter schema
  beyond the artifact wrapper) because a workbench note is expected to be
  disposable.
- **Active hypotheses** — `hypotheses.md`, a lightweight structured list
  (`- [ ] hypothesis text — status: testing/confirmed/rejected`) for
  in-progress reasoning the session wants to track across turns without
  polluting topic memory with half-formed ideas.

### 5.2 TUI panel semantics

The workbench is the one surface designed to be *always visible* rather than
queried on demand — a persistent side panel in the TUI (`harw-tools`'
terminal front-end, once built) showing pinned files, a live tail of
`NOTES.md`, and open hypotheses, updating as the session writes to them.
Unlike diary/palace, which are pull surfaces (an agent explicitly recalls
them), the workbench is a push surface: writes to it are expected to
immediately reflect in whatever's rendering the session, the same way a code
editor's open-tabs bar reflects the working set without being asked. Nothing
in the workbench is retained past project/session archival by default
(`workbench.retention: session-lifetime` default) — it is deliberately the
most disposable surface in the subsystem, the mirror image of the palace.

## 6. Kanban

### 6.1 Typed board model

```
Board { id: BoardId, name: String, lanes: Vec<LaneId>, visibility: VisibilityScope }
Lane  { id: LaneId, board_id: BoardId, kind: LaneKind, bound_worker: Option<AgentId> }
Card  { id: CardId, lane_id: LaneId, title: String, body: String,
        work_id: Option<WorkId>, state: CardState, parents: Vec<CardId>,
        assignee: Option<AgentId>, tags: Vec<String>, visibility: VisibilityScope }
```

`LaneKind` distinguishes a plain triage/todo/done column
(`LaneKind::Status(CardState)`) from a **worker lane**
(`LaneKind::Worker { agent_role: AgentRoleRef }`) — a lane bound to a class of
agent worker rather than to a lifecycle stage. Cards land in a worker lane
when routed to that role; the lane itself is what the dispatcher watches.

### 6.2 Worker lanes: cards become governed work

A card sitting in a worker lane is not yet "work" in the governance sense
until a worker claims it — claiming is exactly `harw-job-runtime::claim`,
producing a `Lease` tied to the card's `WorkId`. This is the direct analogue
of Hermes's dispatcher spawning a profile against a task, reimplemented on
Harwness's own governed-work primitives rather than a bespoke SQLite
dispatcher:

- claim -> `Lease { work_id, holder: AgentId, expires_at, heartbeat_at }`
- heartbeat extends the lease; a dead holder (process gone) triggers reclaim,
  a live-but-slow holder gets the lease extended, matching the
  live-PID-vs-stale-claim distinction that keeps a slow worker from being
  killed prematurely
- retries are `harw-job-runtime`'s existing retry/backoff policy, applied
  uniformly rather than reimplemented per-lane
- a card's `WorkId` is what connects it into the same budget/policy
  machinery as a dream job (§4) — a coding-worker card and a dream job are
  both `Job`s, differing only in `JobKind` and policy profile, which is the
  point of building kanban on the same substrate rather than beside it

### 6.3 Card lifecycle mapped to work states

```
CardState::Triage  -> (decompose, optional)     -> CardState::Todo
CardState::Todo    -> (parents all Done)         -> CardState::Ready
CardState::Ready   -> (claim)                    -> CardState::Running   [WorkId active, Lease held]
CardState::Running -> (complete)                 -> CardState::Done
CardState::Running -> (block)                    -> CardState::Blocked
CardState::Blocked -> (unblock)                  -> CardState::Ready
CardState::Running -> (reclaim: dead/timeout)    -> CardState::Ready     [retry-counted]
CardState::Done | CardState::Blocked -> (archive) -> CardState::Archived
```

Every transition is a `harw-job-runtime` state transition first and a card
render second — the card is a view over `WorkId` state plus kanban-specific
fields (`title`, `body`, `parents`), never an independent source of truth
that could drift from the job ledger.

### 6.4 Approvals on lane transitions

Certain transitions require an approval gate rather than firing unconditionally:

- **Ready -> Running** (claim) on a lane whose bound worker role carries
  `RiskLevel::High` or above requires an `ApprovalRequest` resolved before
  the dispatcher actually spawns the claim — same `ReviewDecision` type
  (`harw-types::roles::ReviewDecision`) used elsewhere in the harness, not a
  kanban-specific approval concept.
- **Running -> Done** on a card tagged `review-required` doesn't auto-flip to
  Done on `complete` — completion instead moves it to a
  `CardState::Blocked { reason: "review-required" }` state pending explicit
  operator sign-off, mirroring the Hermes convention of blocking instead of
  completing for code-changing work, but expressed as a first-class
  `CardState` rather than an overloaded `reason` string.
- **Blocked -> Archived** on a card with unresolved child cards is rejected
  outright (structural invariant, not just an approval gate) — archiving a
  card with live dependents would silently strand them.

### 6.5 TUI `/kanban` view and channel-side summaries

The TUI view renders one column per `CardState`, worker lanes optionally
sub-grouped by bound agent role (mirroring Hermes's "lanes by profile"
toggle, generalized to Harwness's `AgentRole` type rather than a free-text
profile name). Channel-side summaries (for any chat-style front-end Harwness
grows) are a compact digest — counts per lane plus the N most recently
transitioned cards — rendered from the same `KnowledgeIndex` query the TUI
uses, so the two surfaces can't drift by construction, matching the
"CLI/tool/dashboard all route through one layer" discipline already used for
Hermes's kanban.

## 7. Policy & visibility

`VisibilityScope` (defined once, applied to every surface):

```rust
pub enum VisibilityScope {
    /// Only the authoring agent identity can read/write.
    SelfOnly,
    /// The authoring agent and every descendant it spawns (subagent tree).
    DescendantTree,
    /// An explicit allowlist of agent/role identities.
    ExplicitlyGranted(Vec<AgentRoleRef>),
    /// Only a human operator (never an agent) can read/write.
    OperatorOnly,
}
```

Applied per surface:

| Surface | Default scope | Child-agent read | Child-agent write |
|---|---|---|---|
| Core memory | `DescendantTree` | Yes (bootstrap load) | No — only via promotion commit |
| Topic memory | `DescendantTree` (author-set, narrowable) | Yes, within scope | Yes, within own authored scope |
| Palace | `DescendantTree`, promotable to wider `ExplicitlyGranted` | Yes | No direct write — promotion-gated |
| Diary | `SelfOnly` by default (an agent's diary is its own) | Only if `ExplicitlyGranted` | No — diary is first-person, single-author |
| Dream output | `OperatorOnly` by default, downgradable to `DescendantTree` | Only if downgraded | No — dream jobs, not agents, write here |
| Workbench | `SelfOnly` or `DescendantTree` (session vs. project scoped) | Yes if project-scoped | Yes, any session sharing the scope |
| Kanban card | Author-set, commonly `ExplicitlyGranted` (board participants) | Yes if granted | Yes if granted + not lifecycle-locked (Running cards only writable by lease holder) |

The rule that generalizes all seven rows: **read follows the scope; write
additionally requires either being the sole author (SelfOnly surfaces), being
inside the granted set (ExplicitlyGranted surfaces), or going through the
surface's own promotion/lease mechanism** (core memory promotion, palace
promotion, card lease). No surface is agent-writable purely by virtue of
being readable — visibility answers "can see," never "can mutate" by itself.

## 8. Type sketches and command semantics

### 8.1 Core types

```rust
// harw-knowledge/src/artifact.rs

/// Discriminates what kind of durable material an artifact carries; every
/// surface in this document (memory, diary, dream, workbench, kanban) is
/// backed by one `ArtifactKind` variant so the index can query across them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    CoreMemory,
    TopicMemory,
    PalaceNode,
    DiaryEntry,
    DreamReport,
    WorkbenchNote,
    WorkbenchHypothesis,
    KanbanCard,
}

/// One stored unit: frontmatter + body, addressable by a stable id and
/// queryable through `KnowledgeIndex`. This is the unit every write/read
/// path in `harw-knowledge` moves.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeArtifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub frontmatter: Frontmatter,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    pub created_at: jiff::Timestamp,
    pub updated_at: jiff::Timestamp,
    pub visibility: VisibilityScope,
    pub tags: Vec<String>,
    pub links: Vec<ArtifactId>,
    pub author_agent_id: AgentId,
    pub source_session_id: Option<SessionId>,
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Bounded read query — the only path every surface uses to retrieve
/// material, so the "bounded search" guarantee in §2.3 lives in one place.
#[derive(Debug, Clone)]
pub struct RecallQuery {
    pub text: String,
    pub tags: Vec<String>,
    pub kinds: Vec<ArtifactKind>,
    pub max_artifacts: usize,
    pub max_hops: u8,
    pub caller_scope: VisibilityScope,
}
```

```rust
// harw-knowledge/src/kanban/board.rs

pub struct BoardId(pub String);
pub struct LaneId(pub String);
pub struct CardId(pub String);

pub enum LaneKind {
    Status(CardState),
    Worker { agent_role: AgentRoleRef },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardState {
    Triage,
    Todo,
    Ready,
    Running,
    Blocked { reason_kind: BlockKind },
    Done,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Dependency,
    NeedsInput,
    Capability,
    Transient,
    ReviewRequired,
}

pub struct Card {
    pub id: CardId,
    pub lane_id: LaneId,
    pub title: String,
    pub body: String,
    pub work_id: Option<harw_job_runtime::WorkId>,
    pub state: CardState,
    pub parents: Vec<CardId>,
    pub assignee: Option<AgentId>,
    pub tags: Vec<String>,
    pub visibility: VisibilityScope,
}
```

```rust
// harw-knowledge/src/error.rs
use harw_macros::HarwError;

#[derive(Debug, HarwError)]
pub enum KnowledgeError {
    #[msg("artifact {0} not found")]
    ArtifactNotFound(String),

    #[msg("write to {surface} rejected: caller scope {caller:?} does not cover {required:?}")]
    VisibilityDenied { surface: String, caller: String, required: String },

    #[msg("promotion from {from} to {to} requires review but none was recorded")]
    PromotionNotReviewed { from: String, to: String },

    #[msg("card {card_id} cannot archive: {blocking_children} unresolved child card(s)")]
    ArchiveBlockedByChildren { card_id: String, blocking_children: usize },

    #[msg("recall query exceeded bound: {field} = {value} > max {max}")]
    RecallBoundExceeded { field: String, value: usize, max: usize },

    #[from]
    Io(std::io::Error),

    #[from]
    Frontmatter(serde_norway::Error),

    #[from]
    Job(harw_job_runtime::JobError),
}
```

### 8.2 Command semantics (one paragraph each)

**`/memory`** — reads or writes core/topic memory in the caller's scope;
`/memory recall <query>` runs a bounded `RecallQuery` against topic memory
and palace and prints the top `max_artifacts` hits with provenance
(`source_session_id`, tags, and the palace hop path if traversal was
involved); `/memory write <topic> <text>` appends to (or creates) a topic
memory file under the caller's own authored scope — it never touches core
memory directly, since core memory only changes via the promotion commit
step.

**`/diary`** — read-only by default (`/diary show [agent] [date]` renders a
day's entries); an explicit `/diary note <text>` lets an agent append an
out-of-band entry outside the automatic end-of-session/compaction triggers,
useful when an agent wants to record something mid-session without waiting
for a natural trigger point.

**`/dream`** — `/dream run` enqueues a dream job into `harw-job-runtime` with
the configured budget and policy (idle-time default, or immediate with
`--now`); `/dream status` shows currently running/recent dream jobs as
governed work items, same shape as any other `/kanban`-visible job;
`/dream review <job-id>` opens the `DreamReport` for the operator to approve
or reject each proposed topic/palace change individually, which is the only
path those proposals become committed artifacts.

**`/palace`** — `/palace show <node-id>` renders a node with its outgoing
links and computed backlinks; `/palace promote <topic-id>` starts the
topic-to-palace promotion flow (§2.5), landing either as an immediate write
(if the caller's scope authorizes direct promotion) or as a pending review
item; `/palace search <query>` is the graph-aware variant of recall,
returning hits plus their local neighborhood up to `max_hops`.

**`/workbench`** — `/workbench pin <path> [note]` adds a file to the current
session's or project's pinned set with an annotation; `/workbench note
<text>` appends to `NOTES.md`; `/workbench hypothesis add|confirm|reject
<text>` manages the structured hypothesis list; with no subcommand it opens
the TUI panel view described in §5.2 for the active scope.

**`/kanban`** — mirrors the surface familiar from Hermes but over Harwness's
own types: `/kanban create <title> --lane <lane> [--assignee <role>]
[--parent <card-id>]` creates a card (optionally already a `WorkId` if the
lane is a worker lane and the card is created `Ready`); `/kanban show
<card-id>` renders full card state plus its `WorkId`'s job history;
`/kanban claim/complete/block/unblock` drive the lifecycle in §6.3, each a
thin wrapper over the corresponding `harw-job-runtime` call so the card can
never show a state the job ledger disagrees with.

## Open questions

1. **Palace confidence decay** — should `Confidence::Established` nodes ever
   auto-downgrade to `Provisional` on their own (e.g. if no session touches
   them for N months), or should staleness only ever be surfaced (via
   `/palace search` ranking) rather than mutating the artifact? Leaning
   toward surface-only to keep palace writes strictly promotion-gated, but
   worth deciding before `harw-knowledge` v1 locks the `Confidence` enum.

   **Status (Runde 4): decided — surface only, never auto-downgrade.**
   Every dream run looks for staleness candidates
   (`harw_knowledge::dream::palace_stale_candidates`, called from
   `run_maintenance` in `harw-ops/src/dream_run.rs`): `provisional` entries
   unchanged for more than `DREAM_PALACE_STALE_DAYS` = 30 days (promote or
   discard), and `established` nodes that link to a `superseded` entry
   (update the link). Both land only as `maintenance` suggestions in the
   dream report; accepting one only records its status. Changing a node
   stays an explicit `/palace edit|supersede … --confirm`; `established` is
   never downgraded automatically and nodes are never deleted.

2. **Workbench durability for project-scoped surfaces** — §5.2 sets
   `workbench.retention: session-lifetime` as the default, but project-scoped
   workbenches (shared across many sessions over weeks) plausibly need a
   longer retention default than session-scoped ones. Should retention be a
   property of the `WorkbenchScope` variant rather than one global config
   key?

   **Status (Runde 4): decided — retention per scope.**
   `harw_knowledge::workbench::Retention` belongs to the scope
   (`retention.json` in the scope directory), not to a global key. Defaults:
   session scopes expire after 14 days without change
   (`DEFAULT_SESSION_RETENTION_DAYS`), project scopes `keep`. Set with
   `/workbench retention [keep|<tage>d] [--scope=…]`; expired scopes are
   pruned by the maintenance step of every dream run (`prune_expired`). The
   diary, by contrast, has one profile key, `[knowledge.diary]
   retention_days` (default 90; older days move into the monthly rollup).

3. **Dream job budget defaults** — §4.3 leaves `max_tool_calls` "near-zero"
   without a number. Needs a concrete default once `harw-job-runtime`'s
   `Budget` type is implemented and its units (tokens vs. wall-time vs. tool
   invocations) are finalized — this doc can't pick a number in a vacuum.

   **Status (Runde 4): decided.** `[dream] budget` = 16 384 tokens per run
   (profile key, `config-scopes.md` §1.18) plus a fixed wall-clock limit of
   300 s (`DREAM_MAX_WALL`); exceeding either aborts the run before a report
   is written. `max_tool_calls` is effectively 0: the dream turn runs with
   no tools at all (`harw-runtime/src/dream_run.rs`) and only reads a capped
   context from transcripts, diary, topics and palace, framed as untrusted.
   Triggering is `idle_minutes` (15) or a cron `schedule`, with a minimum gap
   of `cooldown_minutes` (60).

4. **Kanban storage under concurrent writers** — §1.1 rejects a database in
   favor of one-file-per-card markdown, consistent with the rest of
   `harw-knowledge`. Hermes's kanban uses SQLite specifically because
   multiple dispatcher/worker processes write concurrently. If Harwness's
   `harw-job-runtime` ever spawns truly separate OS processes (not just
   threads) as worker lanes, file-level locking on `cards/<card-id>.md` needs
   a concrete mechanism (advisory flock? a per-board write-serializing
   actor?) before that becomes a real race instead of a theoretical one.

   **Status (Runde 4): decided — advisory file locks via `fs4`.**
   `harw_knowledge::lock::KnowledgeLock` takes an exclusive `flock` (or
   `LockFileEx`) on a hidden neighbour file `.<name>.lock`, polling with a
   timeout instead of blocking. The lock is released on drop and when the
   holder crashes, so a leftover `.lock` file is never a stuck lock. It
   guards every card file (`board::save_card`; notes and history under the
   card lock), the board lock during `create`, diary appends, workbench
   manifests and palace nodes. A dream run additionally holds
   `dreams/.run.lock`; a second concurrent run ends immediately as busy.

5. **Cross-agent palace visibility beyond `DescendantTree`** — §7 allows
   `ExplicitlyGranted` widening for the palace, but doesn't yet specify who
   is authorized to *grant* that widening (the promoting agent? only an
   operator?). This likely needs to fold into `harw-policy`'s broader
   approval model rather than being decided locally in this document.

   **Status (Runde 4): partly decided.** For the new read tools, agents see
   through `palace.search`/`palace.recall` only `established` nodes, including
   ones with `OperatorOnly` visibility: the operator's explicit
   `/palace promote` is the release to agents. `provisional` topics stay
   invisible to agents. Still open: who may grant an `ExplicitlyGranted`
   widening beyond `DescendantTree`; that still depends on `harw-policy`,
   which does not exist yet.

6. **Diary as evidence in kanban review** — Hermes's convention drops
   structured audit metadata into `kanban_comment` before blocking a card for
   review. Harwness has a diary surface that could serve the same purpose
   more durably (a diary entry survives past the card's archival; a comment
   arguably shouldn't). Should `CardState::Blocked { reason_kind:
   ReviewRequired }` transitions *require* a linked diary entry, or is that
   over-engineering a convention Hermes gets away with as just a convention?

   **Status (Runde 4): partly decided — convention, not a requirement.**
   Cards have their own evidence field (`evidence`, `/kanban evidence
   <karte> <pfad|url>`), timestamped comments with author, and a history;
   the kanban worker writes result and job history to the card. A linked
   diary entry is **not** required for `ReviewRequired`, but a diary entry
   can be linked as evidence by its path. Whether a requirement is worth it
   later stays open.
