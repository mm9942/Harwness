# Harwness Interaction Contract — Master Document

> Status: implemented · Last reviewed: 2026-09-24

Role: this is the single entry point for "how does a human (or another
system) talk to Harwness." It does not redefine anything the three annexes
already specify in detail; it states the unified model, resolves the seams
between them, and is the canonical place to look up the full command
grammar, the full knowledge-surface and plugin/skill sub-grammars, the
unified approval model, and the register of decisions still open across the
whole interaction surface.

Normative annexes (this document supersedes their summaries where the two
disagree; the annexes remain authoritative for everything below the level of
detail reproduced here):

- [`tui-command-contract.md`](./tui-command-contract.md) — command grammar,
  prefix system, keybindings, registry architecture.
- [`channel-ingress-telegram.md`](./channel-ingress-telegram.md) — channel
  abstraction, Telegram binding, admission/pairing, approval rendering.
- [`knowledge-surfaces.md`](./knowledge-surfaces.md) — memory/diary/dream/
  palace/workbench/kanban storage, governance, and per-surface semantics.
- [`secrets-and-audit.md`](./secrets-and-audit.md) — harw-secrets envelope
  design and the tamper-evident audit chain backing approval/command audit.
- [`crates-inventory.md`](./crates-inventory.md) — canonical external
  dependency procurement list for all planned crates.

---

## 1. Overview: one interaction model, two front-ends

Harwness has exactly one command grammar, one permission model, one approval
model, and one knowledge-and-work substrate. It has two *front-ends* onto
that single model:

- the **TUI** (`harw-tui`) — a local terminal, full-fidelity front-end: every
  command, every keybinding, every pager/panel surface.
- **channels** (`harw-channel` + per-platform bindings, Telegram first) —
  remote, capability-limited front-ends reached over a messaging surface,
  admitted through a per-channel perimeter gate, exposing a *subset* of the
  same command grammar under the same permission tiers.

Neither front-end has its own copy of business logic. Both dispatch into the
same `CommandRegistry` (§5 of the TUI annex) against the same session/work/
knowledge state; a channel binding can only ever *narrow* what is reachable
(`CommandScope::ChannelReduced`/`TuiOnly` gating, §1.4 of the TUI annex), it
can never grant something the TUI front-end wouldn't also allow. This is the
same "reduce, never widen" rule the channel annex states for capability
sandboxing (§5 of the channel annex) — the interaction layer and the
capability layer share the same directionality on purpose: a remote surface
is a narrower window onto the same trust boundary, never an alternate one.

```
human (terminal)         human (Telegram, Slack, ...)
      |                              |
   harw-tui                   harw-channel-<binding>
      |                              |
      |---- classify_input() --------|          (§3, TUI annex)
      |                              |
      +---------- CommandRegistry ---+          (§5, TUI annex; one instance)
                     |
        session store / work graph / harw-knowledge / harw-catalog
                     |
              ApprovalRequest/Response (§4, this document)
```

Three things are unified here, not left as three separate documents' problem:

1. **The command grammar** (§2) — the TUI annex's full inventory, with the
   knowledge-surface commands (`/memory`, `/diary`, `/dream`, `/palace`,
   `/workbench`, `/kanban`) and the plugin/skill management commands
   (`/plugins`, `/skills`) worked out to the same subcommand-and-typed-argument
   depth as every other domain, instead of being one-line placeholders.
2. **The approval model** (§4) — one `ApprovalRequest`/`ApprovalResponse`
   pair, one audit record shape, whether the decision arrives via a TUI
   keypress or a Telegram inline-keyboard tap.
3. **The open-decision register** (§6) — every unresolved question raised by
   any of the three annexes, in one numbered list with a status, so nothing
   sits unresolved in three different places without anyone noticing they're
   related.

---

## 2. Full command grammar

Everything in §1 (grammar, `CommandSpec` shape, typed-argument system,
scopes, tiers, output surfaces) and §5 (registry architecture) of the TUI
annex applies unchanged and is not repeated here. This section extends §2
of the TUI annex (the command inventory) in the two places it was left
incomplete: §2.6 (knowledge surfaces, one-liners only) and the two catalog
commands that need full sub-grammars (`/plugins`, `/skills`).

### 2.1 Typed-argument additions

The TUI annex's typed-reference table (§1.3) already carries `SkillRef`. This
document adds the remaining references needed to give the knowledge and
plugin/skill domains the same typed-argument treatment as work items and
sessions:

| Type | Surface form | Example | Resolved by |
|---|---|---|---|
| `BundleRef` | bare name or `bundle:<name>@<version>` | `my-tools`, `bundle:acme-toolkit@2` | catalog bundle registry |
| `CardRef` | `card-<n>` or bare `CardId` | `card-77` | kanban board index |
| `LaneRef` | bare lane name, `board:<board-id>/<lane>` when disambiguation is needed | `worker/coding`, `board:acme/triage` | kanban board index, defaulting to the active board |
| `BoardRef` | bare board name | `acme` | kanban board registry |
| `AgentRoleRef` | `role:<name>` or bare name where unambiguous | `role:coding-worker` | agent-role catalog |
| `ArtifactRef` | bare `ArtifactId` (kind-prefixed slug) | `topic/deploy-pipeline`, `palace/on-call` | `KnowledgeIndex::get` |

Corresponding `ArgType`/`ResolvedArg` variants (`Bundle`, `Card`, `Lane`,
`Board`, `AgentRole`, `Artifact`) and `ReferenceResolver` methods
(`resolve_bundle`, `resolve_card`, `resolve_lane`, `resolve_board`,
`resolve_agent_role`, `resolve_artifact`) are added alongside the existing
ones in §5.1/§5.3 of the TUI annex, following the same "structured
`UnresolvedRef` over generic parse failure" rule.

### 2.2 Knowledge surfaces — full subcommand grammar

Supersedes TUI annex §2.6 (which reserved the names only). Tiers use the
TUI annex's `PermissionTier`; parity uses `CommandScope`.

> **Current state (2026-09-24).** The tables below describe the grammar
> the operations in `harw-ops/src/{memory,diary,dream,palace,workbench,kanban,learn,matrix}/…`
> actually parse. Every operation is declared `permission = "operator"`; the
> Tier column keeps the finer read/write/maintenance distinction of the
> original contract. Parity is declared **per operation** in code (one
> `visibility` per `#[operation]`), so a per-subcommand `-` below means
> "meaningless or refused from a channel", not a separate registry entry.
> The `busy` column uses the classes from §2.6.3 (`immediate` = `Immediate`,
> `—` = deferred until the running turn ends). Model-facing read tools for
> the same surfaces are listed at the end of this section.
>
> The read forms of `/kanban`, `/dream` and `/diary` carry
> `busy_subcommands` on their operations and run immediately during a turn;
> writing forms defer. The classification table test in
> `harw-tui/src/command_exec.rs` (`EXPECTED_BUSY_CLASSES`) is authoritative.

**`/memory`** — durable cross-session agent memory (v2 HOT/WARM/COLD layer
plus the v3 fact store; `harw-ops/src/memory.rs`). Op parity: `Y`.

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/memory` / `list` | — | Obs | Y | — | HOT tier (≤100 lines) |
| `/memory stats` | — | Obs | Y | — | Counters over HOT/WARM/COLD and open signals |
| `/memory recall` | `<stichwort…>` | Obs | Y | — | Searches project facts before global facts, with provenance |
| `/memory record` | `<text…> [--project\|--global]` | Op | Y | — | Writes a fact (default `--project`) |
| `/memory forget` | `<name>` | Op | Y | — | Removes the fact from project and global roots |
| `/memory promote` | `<fact-id> [--project\|--global] [--slug <slug>]` | Op | Y | — | **Bridge fact → topic.** Writes `knowledge/topics/<slug>.md` as palace status `provisional` with origin `{kind: "fact", …}`; never overwrites an existing topic (use `--slug`). `established` only via `/palace promote`. |
| `/memory maintain` | — | Maint | Y | — | Idempotent v2 consolidation |
| `/memory consolidate` | `[--project\|--global]` | Maint | Y | — | Deterministic phase-2 consolidation only |
| `/memory topics` | — | Obs | Y | — | `provisional` topics visible to the caller as `{slug, title, status, origin}` (source for the promote offer) |

The original `/memory write <topic> <text>` and `/memory show <ArtifactRef>`
were not built: topics are created through `/memory promote` or an accepted
dream suggestion, and read through `/palace show`.

**`/diary`** — per-agent narrative log (`harw-ops/src/diary.rs`). Op parity: `R`.

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/diary` / `today` | — | Obs | R | immediate | Caller's own diary, today (UTC) |
| `/diary show` | `[AgentRef] [--date=YYYY-MM-DD]` | Obs | R | immediate | One day |
| `/diary show` | `[AgentRef] --from=YYYY-MM-DD [--to=YYYY-MM-DD]` | Obs | R | immediate | Range view (bounded number of days); not combinable with `--date` |
| `/diary search` | `<text> [--agent=<id>] [--from=…] [--to=…]` | Obs | R | immediate | Case-insensitive full-text search over the structured entries of all visible diaries; bounded hits, newest first |
| `/diary note` | `<text>` | Op | R | — | Out-of-band entry (`DiaryTrigger::Manual`) |
| `/diary agents` | — | Obs | R | immediate | Agents with diary entries (`{"agents":[…]}`); a non-operator caller only sees itself. Used by the browser's agent picker |

The `#` prefix shortcut (§3, TUI annex) is sugar for `/diary note` on both
front-ends — this document confirms that mapping is exact, not merely
similar, so `#` and `/diary note` share one code path and one audit trail.

Automatic entries  do not go through the command:
`harw-runtime/src/diary_wiring.rs` records `compaction` (after a
model-summarised compaction) and `end-of-session` (close, quit, `/new`,
`/resume`, one-shot end) for the root session's agent; a dream run adds
`dream-reflection` entries only when a `diary_reflection` suggestion is
accepted. Visibility is the real caller's (operator → `OperatorOnly`, agent
→ `SelfOnly`); an invisible day reads like a missing one.

**`/dream`** — idle-time reflection as a governed job (`harw-ops/src/dream.rs`,
`harw-ops/src/dream_run.rs`). Op parity: `R`.

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/dream` / `list` | — | Obs | R | immediate | Visible dream reports, newest first, with proposal counts |
| `/dream show` | `<id>` | Obs | R | immediate | One report; `<id>` = `dream/<date>/<work-id>`, `<date>/<work-id>` or an unambiguous work id |
| `/dream run` | — | Maint | R | — | Starts one run through the same core as the gateway scheduler (lock, `JobKind::Dream` in the job ledger, structured JSON output with one repair turn, budget from `[dream] budget`, maintenance). Only where the runtime provides a dream launcher (TUI, one-shot) |
| `/dream status` | — | Obs | R | immediate | Scheduler state from `dreams/state.json` and `[dream]`: enabled, trigger, last/next run, running?, ledger state of the last job, open suggestions |
| `/dream review` | `[<id>]` | Obs | R | — | Open suggestions of all reports or one report |
| `/dream review` | `<id> accept\|reject <p-id> [reason…]` | Maint | R | — | Decides one suggestion. `accept` runs the write path: `topic`/`palace` → topic `provisional` (as `/memory promote`), `diary_reflection` → diary entry, `skill_idea`/`agent_idea` → a `/learn` proposal (never live), `follow_up`/`maintenance` → status only |

The earlier sketch `/dream run [--now] [--budget=Duration]` and
`/dream review <WorkId> [--approve=…] [--reject=…]` is replaced by the grammar
above; the budget is configuration (`[dream]`, `config-scopes.md` §1.18),
not a flag. A dream never writes knowledge directly: every suggestion is
stored `pending` in the report's structured side file until a human decides.

**`/palace`** — the linked long-term memory graph (`harw-ops/src/palace.rs`).
Op parity: `Y`.

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/palace` / `list` | — | Obs | Y | immediate | All visible nodes |
| `/palace show` | `<ArtifactRef>` | Obs | Y | immediate | Node body + outgoing links + computed backlinks (filtered to visible sources) |
| `/palace search` | `<query> [--max-hops=n] [--max=n]` | Obs | Y | immediate | Graph-aware recall; bounded per §2.3 of the knowledge annex |
| `/palace promote` | `<topic-ref>` | Maint | Y | — | Topic → palace node, lands `established` (the explicit command is the review) |
| `/palace supersede` | `<alt> <neu> [--confirm]` | Maint | Y | — | `alt` becomes `superseded` and points to `neu` |
| `/palace edit` | `<id> <text…> [--confirm]` | Maint | Y | — | Replaces the body; `[[wikilinks]]` become frontmatter links |
| `/palace link` | `<a> <b> [--confirm]` | Maint | Y | — | Adds a `[[palace/b]]` reference to `a` |

Review gate for the review-gated write paths: operator only (an agent principal
gets `NotAvailable`); a `provisional` node may change freely; a change to an
`established` node is a new revision and requires `--confirm` (the previous
version is appended to `palace/<slug>.history.jsonl`); a `superseded` node is
frozen. Nodes are never deleted. Index reads use an mtime-invalidated cache.

**`/workbench`** — persistent scratch/working-set surface
(`harw-ops/src/workbench.rs`). Op parity: `R` (the operation carries one
visibility; `pin` and the bare panel are still meaningless from a channel).

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/workbench` / `show` | `[--scope=session\|project\|project:<slug>]` | Obs | - | immediate | Panel text plus structured data: pins with status (`present\|changed\|missing\|not_a_file`) and preview, hypotheses, notes |
| `/workbench pin` | `<path> [note]` | Op | - | — | Resolved against the TUI's working directory |
| `/workbench unpin` | `<path>` | Op | - | — | |
| `/workbench note` | `<text>` | Op | R | — | Appends to `NOTES.md` |
| `/workbench note edit` | `<n> <text>` | Op | R | — | `<n>` = `#<n>`/`<n>` (1-based) or the note's timestamp |
| `/workbench note rm` | `<n>` | Op | R | — | |
| `/workbench hypothesis` | `add\|confirm\|reject <text>` | Op | R | — | `confirm`/`reject` by exact text or `#<n>` |
| `/workbench retention` | `[keep\|<days>d]` | Op | R | — | Shows or sets the scope's retention; defaults: session scopes expire after 14 days without change, project scopes `keep` |

Every subcommand accepts `--scope=` (also `--scope <s>`); without it the
active session is meant, `--scope=project` without a slug means the project
of the working directory. Expired scopes are pruned by the maintenance step
of a dream run.

**`/kanban`** — visual board projection of the work graph
(`harw-ops/src/kanban.rs`). Op parity: `Y`. `--board=<b>` is accepted
globally; `LaneRef` may be `board:<b>/<lane>`.

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/kanban` / `list` / `show` | `[--board=<b>] [--all]` | Obs | Y | immediate | Cards per lane (worker lanes carry role and risk); `--all` includes archived |
| `/kanban boards` | — | Obs | Y | immediate | All boards |
| `/kanban create` (`add`) | `<title> [--lane=<LaneRef>] [--assignee=<AgentRoleRef>] [--parent=<CardRef>]… [--tag=<t>]… [--board=<b>]` | Op | Y | — | Without `--lane` → `triage`; `--assignee=<role>` routes to `worker/<role>` and creates the job (`Todo`, `Ready` once all parents are `Done`) |
| `/kanban show` | `<CardRef>` | Obs | Y | immediate | Full card state, ledger snapshot, comments, evidence, history, result |
| `/kanban edit` | `<CardRef> <text…>` | Op | Y | — | Replaces the card body |
| `/kanban comment` | `<CardRef> <text…>` | Op | Y | — | Comment with time and author |
| `/kanban evidence` | `<CardRef> <path|url>` | Op | Y | — | Evidence reference (`evidence: [..]`) |
| `/kanban approve` | `<CardRef> [note…]` | Op | Y | — | Releases a worker card waiting in `blocked (AwaitingApproval)`; the job worker then starts the role agent |
| `/kanban reject` | `<CardRef> [reason…]` | Op | Y | — | Archives the waiting card (job cancelled) |
| `/kanban todo` / `ready` | `<CardRef>` | Op | Y | — | `Triage → Todo`, `Todo → Ready` (parent gate) |
| `/kanban claim` | `<CardRef>` | Op | Y | — | Explicit operator claim; the command itself is the approval (`ApprovedOnce`), risk from the role table |
| `/kanban complete` (`done`) | `<CardRef>` | Op | Y | — | |
| `/kanban block` | `<CardRef> <BlockKind>` (also `--reason=`) | Op | Y | — | |
| `/kanban unblock` | `<CardRef>` | Op | Y | — | Refused on cards awaiting approval, so unblocking is never mistaken for approval |
| `/kanban archive` | `<CardRef>` | Maint | Y | — | |
| `/kanban move` | `<CardRef> <todo\|ready\|running\|done\|blocked\|archived> [<BlockKind>]` | Op | Y | — | TUI alias that picks the matching lifecycle transition |

Kanban worker : the job worker of `harw serve` picks up `Ready`
cards in worker lanes but **never starts an agent without approval**: it
blocks the card with `AwaitingApproval` and records "approval requested" in
the history. Risk per role comes from the role's registry profile (read-only
roles `Low`, writing roles `Medium`, process/secret/plugin rights and unknown
roles `High`, fail-closed). Result and job history are written to the card.
Dream runs are recorded as `JobKind::Dream` in the job ledger. A mirror card
on a `dream` board (plan D5) was **deliberately dropped**: nothing is put on
the kanban board automatically; Kanban is used only on the user's explicit
request. `kanban.list`/`kanban.show` are offered only to the UIA root and the
root orchestrator, always ask for approval, and their tool descriptions carry
that usage rule.

**`/learn`** — learning loop, proposals only (`harw-ops/src/learn.rs`). Op parity: `R`.

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/learn` / `scan` | — | Op | R | — | Scans the current session and files proposals |
| `/learn note` | `<text…> [--target memory\|skill\|agent]` | Op | R | — | Proposal from text; `skill` goes through the skill-proposal store (`/skills accept`) |
| `/learn list` | `[--all]` | Obs | R | — | Open (or all) proposals |
| `/learn show` | `<id>` | Obs | R | — | |
| `/learn accept` / `reject` | `<id> [reason…]` | Op | R | — | **Marks only** (targets `memory`/`agent`) and names the command the operator runs to apply it; `/learn` never writes a fact, skill, agent or context program itself |

**`/matrix`** — matrix game (`harw-ops/src/matrix/mod.rs`; design in
`matrix-game.md`). Op parity: `R`. Only the later additions are listed;
the full grammar (`step`, `auto N`, `pause`, `inject`, `override`, `veto`,
`reveal`, `fork`, `end`, `replay`, `show`, `list`) is in the module doc.

| Subcommand | Args | Tier | Parity | busy | Notes |
|---|---|---|---|---|---|
| `/matrix start` | `<scenario-id|path> [--seed N] [--package <id>]` | Op | R | — | `--package` loads an inject package from the scenario library |
| `/matrix compare` | `<run> <run> […]` | Obs | R | — | Compares two or more runs (e.g. with and without a package) |
| `/matrix show` / `list` | — | Obs | R | immediate | Panel data (also F9) |

**Model-facing read tools.** Agents may *read* the knowledge
surfaces through read-only tools; all writes stay operator commands or pass
through review:

| Tool | Scope |
|---|---|
| `workbench.show` | The session's own workbench (plus the project scope of the bound workspace); a context provider also places pins and hypotheses, capped, into the session context |
| `diary.read` | Only the calling agent's own entries, by date range, capped |
| `palace.search`, `palace.recall` | Only `established` nodes, bounded hops |
| `kanban.list`, `kanban.show` | Read-only board and card view |

`workbench.note`/`workbench.hypothesis` remain the only model-side writes
to a knowledge surface (no path pinning). The read tools are registered only
for profiles whose rights admit them (`ReadWorkspace`); `NoTools` and the
Telegram workspace profile (`WorkspaceEdit`) get none of them.

### 2.3 `/plugins` — extension bundle management

`/plugins` in the TUI annex (§2.5) was a one-line placeholder
(`[--list | --enable=name | --disable=name]`). This document replaces it with
the full subcommand grammar below; the flag-based form in the annex is
superseded.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/plugins list` | `[--enabled-only]` | Obs | Y | Lists bundles from the current `CatalogSnapshot` (§8.1 of the knowledge annex's sibling `harw-catalog` crate) with enabled/disabled state |
| `/plugins inspect` | `<BundleRef>` | Obs | Y | Manifest contents: declared tools, skills, MCP servers, requested capability grants |
| `/plugins install` | `<path\|url> [--yes]` | Owner | TuiOnly | See §2.3.1 — manifest validation, policy check, mandatory operator confirmation; `--yes` still requires the confirmation prompt to have been shown in the same invocation, it is not a silent-skip flag |
| `/plugins enable` | `<BundleRef>` | Owner | TuiOnly | Affects only runs started after this point — see §2.3.2 |
| `/plugins disable` | `<BundleRef>` | Owner | TuiOnly | Same immutable-snapshot rule as `enable` |
| `/plugins remove` | `<BundleRef>` | Owner | TuiOnly | Deletes the bundle from the catalog source; a bundle in use by a still-running `CatalogSnapshot` continues to work until that run ends |
| `/plugins verify` | `<BundleRef> [--digest] [--provenance]` | Obs | Y | Recomputes and checks the bundle's content digest and provenance attestation; the same check `/doctor --section=catalog` runs across every installed bundle |

#### 2.3.1 Install flow: never a silent privilege widening

`/plugins install` always runs, in order:

1. **Manifest validation** — schema-checked, requested capability grants
   enumerated explicitly (tools, MCP servers, filesystem/network scope
   requests); a manifest that requests something the schema doesn't know how
   to express is rejected, not best-effort accepted.
2. **Policy check** — `harw-policy` evaluates the requested grants against
   the operator's own policy ceiling; a bundle cannot request more than the
   installing operator's own tier could grant directly.
3. **Operator confirmation** — the full, human-readable capability diff is
   printed (*"this bundle will be able to: read/write `sandbox.network`,
   invoke tool `fs::write_file`, register MCP server `acme-internal`"*) and
   requires an explicit `y`/`yes` response in the same TUI session. There is
   no channel path for this step at all — `/plugins install` is `TuiOnly`
   precisely so this confirmation cannot be rushed through a chat reply.

Steps 1–3 are the same three steps `/doctor --section=catalog` re-runs on
every already-installed bundle (verification is a subset of installation's
own checks, run again, not a separate mechanism).

#### 2.3.2 Enable/disable and `CatalogSnapshot` immutability

`harw-catalog` builds one `CatalogSnapshot` at the start of each run (session,
job, or dream job) and that snapshot does not change for the life of the run
— this is inherited, not introduced here, but it is the reason
`/plugins enable`/`disable` are documented as affecting *later* runs only.
Disabling a bundle mid-run does not retroactively strip a capability from a
session already holding it; the session's snapshot is what it is until the
session ends. A `/doctor` check flags any long-lived session still running
against a stale snapshot after a disable, so operators are not surprised by
this, but the mechanism is deliberate: yanking a capability out from under a
live tool call mid-execution is a worse failure mode than a bounded delay
before the change takes effect.

### 2.4 `/skills` — skill management

`/skills` in the TUI annex (§2.5) was also a placeholder
(`[SkillRef] [--search=text] [--install]`); this supersedes it.

| Subcommand | Args | Tier | Parity | Notes |
|---|---|---|---|---|
| `/skills list` | `[--source=bundle\|workspace\|user] [--search=text]` | Obs | Y | Source is one of the three provenance tiers a skill can come from |
| `/skills show` | `<SkillRef>` | Obs | Y | Renders the skill's declared inputs, budget defaults, and source |
| `/skills run` | `<SkillRef> [args...]` | Op | R | Executes as a governed work item (`WorkId`, `Budget`, same job-runtime primitive as a dream job or a kanban worker card) — see §2.4.1 |
| `/skills reload` | — | Maint | TuiOnly | Rebuilds the skill catalog into a new `CatalogSnapshot`; running runs are untouched (same rule as §2.3.2) |
| `/skills new` | `<name>` | Maint | TuiOnly | Scaffolds a new user-tier skill under the workspace's skill directory |
| `/skills enable` | `<SkillRef>` | Owner | TuiOnly | |
| `/skills disable` | `<SkillRef>` | Owner | TuiOnly | |

#### 2.4.1 `run`: reduced parity means approval-gated, not unavailable

`/skills run` is reachable from a channel, but only with an `ApprovalRequest`
resolved first (§4) — this is what "reduced channel parity (run only with
approval)" means concretely: the command is admitted, dispatched, and turns
into a governed work item exactly as it would from the TUI, but the
dispatch step is preceded by the same approval flow a `RiskLevel::High`
kanban claim goes through (knowledge annex §6.4), rendered as a Telegram
inline keyboard or a TUI `/review` prompt depending on which front-end asked.
A TUI-invoked `/skills run` may skip the prompt only if the resolved policy
for that skill/tier combination doesn't require one; the *reduction* on
channels is that the approval step can never be skipped there, regardless of
policy, since a remote caller's identity confidence is inherently lower.

### 2.5 Updated inventory totals

Counting every subcommand in §2.2–§2.4 as one command (consistent with how
the TUI annex counts `/export`'s flags as one command, not several):

- TUI annex base inventory (§2.1–§2.5, §2.7–§2.8, excluding the six
  knowledge-surface placeholders and the two placeholder catalog rows that
  are now superseded): **44 commands**
  (56 total in the annex − 6 knowledge one-liners − 2 superseded
  `/plugins`/`/skills` placeholder rows, `/export` counted once).
- Knowledge surfaces (§2.2 above): **22 subcommands**
  (`/memory` 3, `/diary` 2, `/dream` 3, `/palace` 3, `/workbench` 4,
  `/kanban` 7).

- `/plugins` (§2.3): **7 subcommands**.
- `/skills` (§2.4): **7 subcommands**.

**Grand total: 44 + 22 + 7 + 7 = 80 commands/subcommands** in the unified
grammar, up from the TUI annex's headline count of 56 (which undercounted
the knowledge surfaces at one line each and left both catalog domains as
flag-based placeholders rather than full subcommand sets).

### 2.6 Current state (2026-09) — present in code, contract caught up

The following commands already exist in `harw-ops/src/` and are now captured
here, one line each, purpose from the module doc comment:

- **`/analyze`** — bottom-up analysis of a workspace via analyst child
  agents, fanning out crate-level by crate-level with a final synthesis.
- **`/add-workdir`** — grants additional working directories for the running
  session (registers them in the shared `ExtraRootsCell`).
- **`/bug-report`** — manually triggers a local bug report, written via
  `harw_ops::bug_report::write_bug_report`.
- **`/diff`** — a read-only git-diff operation, delegating exclusively to
  `shell.exec` through a hardened `GitDiffPlan`.
- **`/explore`** — asks a bounded question of a read-only child
  (`explorer`) and validates the result against the `ResearchFinding`
  contract.
- **`/context-proposal`** — review surface for `ContextProposal`: list, view,
  accept, reject (accepting only marks it; it does not change a context
  program).
- **`/goal`** — a goal operation over `harw-plan`; describes the desired end
  state, `achieve`/`abandon` are command-only (not exposed to the model).
- **`/mode`** — shows and requests a change of interaction mode (`chat`,
  `plan`, `explore`, `work`, `shell`); TUI-only, not a model tool, so a
  model cannot raise its own tool ceiling.
- **`/research`**, **`/research-deps`**, and **`/research-web`** — bounded
  research through children; same structure, differing only in the child
  role and default source classes. `/research` is general research
  (`researcher`), `/research-deps` checks Rust dependencies
  (`researcher-deps`, or language-neutral via `dependency-researcher` with
  `--generic`), `/research-web` does egress-bound research on the network.
- **`/plan`** — a plan operation over `harw-plan`/`harw-plan-bridge`, both a
  command and a model tool; every mutation runs through `PlanStore::apply`.
  The store holds multiple plans, exactly one active (`list`, or in the TUI
  `/plan plans`, `switch`, `archive`, `inspect [id]`). A plan proposed from
  the model surface starts as a proposal; `submit` presents it in the TUI
  for confirmation, only after which it is binding and tied to a goal.
  `step <id> <open|running|done|blocked> [evidence]` reports progress,
  `done` requires evidence. The TUI separately intercepts plan **files** in
  plan mode locally: `/plan` (enters plan mode), `/plan show|list|open|edit`
  over `.harw/plans/<slug>.md`.
- **`/btw`** (TUI-local) — a throwaway side question about the
  conversation: a single model call over a snapshot of the history, no
  tools, without touching the running turn. Neither the question nor the
  answer lands in the history or the input history; Esc cancels, 60s time
  limit. Other entry points (Telegram) reply with a decline.
- **`/usage`** — shows recorded token usage and guard events for the current
  session from the `StateStore` snapshot.
- **`/stop`** — aborts a running job in a controlled way; a command and a
  model tool with `approval = "always"`.
- **`/sandbox-lease`** (user decision 2026-09-21) — a command **and** a
  model tool for direct host approval of `shell.exec`. The model tool
  (`harw-ops/src/sandbox_lease.rs`, `model_tool` without extra approval,
  since the dialog itself *is* the approval; argument `reason`) triggers a
  `HostPermitPrompt` (pre-selected `SessionLease`) and waits up to 300s:
  `SessionLease` -> `mark_global_approval` (all `shell.exec` calls of the
  harw process then run on the host); `SingleExecution` ->
  `mark_global_single_use` (only the next call); rejection/timeout return an
  error text. **A host lease has no expiry and only the user can end it**
  (user decision 2026-09-24): `Ctrl+H` in the TUI (`end_host_mode`) or the
  typed command `/sandbox-lease revoke`. The command
  `/sandbox-lease [status|revoke]` (`busy = "immediate"`) is purely local:
  `status` shows the active grant, `revoke` ends it immediately via
  `revoke_global_approval` + `revoke_session_approval` +
  `ledger.revoke_session`. The model tool does not offer `revoke`, and a
  model call with `action = "revoke"` is rejected ("only the user can end
  host mode"): the op accepts `revoke` only when its `OpContext` carries the
  `HostLeaseUserControl` marker, which the runtime places exclusively in the
  slash-command `ServiceMap` — it is not an argument, so the model cannot
  forge it. `/status` (`harw-ops/src/status.rs`) additionally shows a
  "Host-Lease: aktiv (bis Strg+H oder /sandbox-lease revoke) / Einmalfreigabe
  / aus" line. Details of host execution itself live in
  `mediated-process-execution.md`.
- **`/permissions`** — an overview of workspace identity, sandbox rights,
  approval mode, and allow/deny rules: `allow|deny <tool> [pattern]
  [--session|--project|--user]` (`--user` = `--global`; a pattern is a
  `match` for shell or a `path` for files), `rules` (rules with their
  origin), `rm <n>` (short for `remove`), and `log [count]` (the most recent
  auto-mode decisions, along with the current security-ceiling state).
  Check order: `ALWAYS_ASK_TOOLS` first, then deny before allow before the
  classifier; deny rules also apply in `ask` mode. An allow rule for a tool
  in `ALWAYS_ASK_TOOLS` is rejected. In `full` nothing asks, not even
  `ALWAYS_ASK_TOOLS`; a deny rule then refuses the call instead of asking.
- **`/agent`** — the agent tree with live values (root "UIA · <name>"),
  `list`, `stop`, `budget`, `use`. The TUI intercepts locally:
  `stream <orchestrators|all|none>` (live-stream children into the
  history), `bg` (background agents with progress), and `cancel <id>`
  (cancel one's own background agent). The earlier TUI command `/agents` no
  longer exists.
- **`/effort`** — sets the provider-neutral reasoning strength for
  subsequent turns, live, for the running session; operator-only, TUI-only,
  not a model tool. Grammar: `show | clear | minimal | low | medium | high
  | xhigh | max` (alias `/reasoning`) — has a structural twin,
  `/uia-effort` (see §2.6.1).

### 2.6.1 Model/provider/effort commands (current, 2026-09)

Complete command inventory for the model, provider, and reasoning-effort
axis, as implemented in `harw-ops/src/model.rs`, `provider.rs`, and
`effort.rs`. `/provider switch` and `/uia-provider switch` **no longer
exist** — a provider+model switch runs exclusively, atomically, through
`/model switch <id>` or `/uia-model switch <id>`, which resolve the target
model's configured provider and delegate to
`provider::handle_switch_core`/`handle_uia_switch_core`, even when the
target model belongs to a different provider than the currently active one.

| Command | Grammar | Effect | Visibility | Permission |
|---|---|---|---|---|
| `/model` (alias `/m`) | `show \| list \| switch <id>` | `switch` changes provider+model **atomically**, live, via the `SessionController` | `tui_only` | `operator` |
| `/uia-model` | `show \| list \| switch <id>` | like `/model`, but for the pinned UIA selection (`uia_provider`/`uia_model`); `switch` is live | `tui_only` | `operator` |
| `/uia-worker-model` | `show \| list \| switch <id>` | `switch` validates the model ID against the *effective* UIA provider and only persists `uia_worker_model` in the profile `config.toml` — **no** live switch, no separate `uia_worker_provider` concept (the worker shares its provider with the UIA); takes effect from the next session | `tui_only` | `operator` |
| `/effort` (alias `/reasoning`) | `show \| clear \| minimal \| low \| medium \| high \| xhigh \| max` | a live session setting via the `SessionController`, applies immediately to subsequent turns | `tui_only` | `operator` |
| `/uia-effort` | same grammar as `/effort` | persists `reasoning.uia` in the profile `config.toml`, **no** live override (a deliberate difference from `/effort`), takes effect from the next session | `tui_only` | `operator` |
| `/provider` (alias `/p`) | `show \| list \| test` | read-only; `switch` falls into the unknown-subcommand branch and points to `/model` | `tui_only` | `operator` |
| `/uia-provider` | `show \| list \| test` | like `/provider`, for the UIA pin selection; `switch` points to `/uia-model` | `tui_only` | `operator` |
| `/provider-concurrency` | `<ProviderRef> [<n \| unlimited>]` | without a value: shows the provider's load state (concurrency, rate-limit wait incl. the shared HTTP-429 cooldown, observed 429s); with a value: adjusts a provider's `max_concurrency` **live** via the `DynamicConcurrencyLimiter`: raising it releases permits immediately, lowering it is lazy (running requests are never aborted, only refill is throttled until the target is reached); recommendation on repeated HTTP 429s: lower, don't raise | `tui_only` **and** `model_tool` (the UIA can throttle itself) | `operator` (command); `approval = "always"` for the tool call (the macro only supports one static approval level per `model_tool`, so it applies to lowering too) |

**Visibility of the concurrency limit:** `/provider show` (via the shared
`format_load_status` helper, `harw-ops/src/provider.rs`) and `/status`
(`harw-ops/src/status.rs`) both show the same four values from
`harw_provider_http::ProviderLoadStatus`: the concurrency limit
(`unlimited` if no limiter is installed), the number of free permits, the
current rate-limit wait, and the number of HTTP 429 responses observed
since start (with a note to lower rather than raise on repeated 429s). The
`DynamicConcurrencyLimiter` is the same mechanism for the OpenAI-compatible
**and** the Anthropic path —
`AnthropicMessagesProvider::configure_concurrency` installs the same
limiter type as the OpenAI-compatible provider
(`harw-provider-http/src/anthropic.rs`).

**Note on granularity:** `OperationMeta.busy` is a property of the whole
operation, not of the subcommand — at that level `/model` and `/provider`
are `busy = "immediate"` as a whole. The TUI refines this at runtime via
`busy_availability_for` (`harw-tui/src/command_exec.rs`): only `show`/`list`
(and the bare `/provider`, equivalent to `show`) actually dispatch
immediately during a running turn; `/model switch <id>`, bare `/model`
(opens the picker), and `/provider test` are queued until turn end despite
`OperationMeta.busy = "immediate"`. This matches the originally narrower
scope of the user decision ("only `show`") — not every subcommand of these
two operations is immediately available, see §2.6.3.

### 2.6.2 UIA / orchestrator separation (provider level)

- Correction (previously described as "two independent provider clients"):
  there is **one** router across every enabled provider
  (`RoutingModelProvider`, built once in
  `harw-provider-http/src/lib.rs::build_provider_with_load_registry`, with
  exactly one HTTP client per enabled provider). The UIA no longer gets its
  own client through this, only its own default route:
  `build_uia_model`/`build_uia_worker_model` (`harw-runtime/src/model.rs`)
  wrap the shared router with `UiaDefaultRouteProvider`, which fills in a
  request's `provider_id`/`model_id` from `uia_provider`/`uia_model` **only
  when** the request doesn't already carry them — a live choice via
  `/uia-model switch` (which sets the request's own `provider_id`/`model_id`
  explicitly) always takes precedence over the default route. The
  `build_uia_model` doc comment notes: "since unification with the default
  router, [construction] can no longer fail — no second HTTP client is
  built." When `uia_provider` diverges from `default_provider`, UIA requests
  no longer fail at runtime as a result. The concurrency limit
  (`DynamicConcurrencyLimiter`, §2.6.1) is **one** limit per provider, not
  per route — a UIA request over the default route and an orchestrator
  request to the same provider share the same quota.
- `uia_worker_model` pins only the `model_id` via a `PinnedModelProvider`,
  **never** the `provider_id` — the provider always stays the same as the
  effective `uia_provider`.
- The entire `uia-worker` role family always runs as **exactly one
  instance, never in parallel**: `harw-core/src/child_controller.rs` and
  `harw-core-bridge/src/agent_tool.rs` cap `slots`/`max_parallel` for these
  roles internally at `1`, regardless of the caller's parameter (an
  `analyze(max_parallel: 4)` cannot bypass this). The UIA root session
  itself cannot currently spawn a second concurrent instance as a child
  session anyway (there is no matching spawn code path).

### 2.6.3 Busy availability during a running turn

(`harw-operations/src/operation.rs`, `harw-tui/src/command_exec.rs`,
`harw-tui/src/app.rs`, `harw-tui/src/app/busy_queue.rs`.)

**Three classes** (`BusyAvailability`):

| Class | Behavior during a turn |
|---|---|
| `Immediate` | runs right away; pure read or control commands with no session change across the turn boundary |
| `Staged` | runs right away, but the change is only queued in the `SessionController` or a config cell and takes effect **from the next turn**; feedback: "— applies from the next turn" |
| `DeferredUntilTurnEnd` (default) | queued and executed after turn end through the same authorized command channel |

**Declared on the operation.** `#[operation(command(busy = "…",
busy_subcommands = "…"))]`: `busy` is the operation's class,
`busy_subcommands` overrides it per subcommand (first argument token), e.g.
`busy_subcommands = "show=immediate, list=immediate, -=immediate"` (`-`
stands for the bare form). `Operation::busy_for(args)` and
`BusySubcommand::resolve` evaluate this; the special cases for `/model` and
`/provider` that used to be hardcoded in `busy_availability_for` are gone as
a result.

**Classification (excerpt; authoritative is `EXPECTED_BUSY_CLASSES` in
`harw-tui/src/command_exec.rs`, a table test over the whole catalog):**

| Class | Commands |
|---|---|
| `Immediate` | `/help` `/status` `/ps` `/usage` `/diff` `/work` `/review` `/approve` `/deny` `/cancel` `/stop` `/agent` `/models` `/attach` `/sandbox-lease` `/provider-concurrency`; `/provider` (except `test`); `/plugins` (except `install`/`activate`/`uninstall`); read forms: `/permissions` bare/`show`/`mode`/`set`, `/skills` bare/`list`/`show`, `/workbench` bare/`show`, `/diary` bare/`show`/`today`/`search`, `/palace` bare/`list`/`show`/`search`, `/matrix show`/`list`, `/model show`/`list`, `/effort`/`/mode`/`/uia-*` bare or `show`/`list`; `/permissions rules`/`log`; with an active planning gate, `/plan` bare/`inspect`/`ready`/`waves`/`list` and `/goal` bare/`show`/`check`; without a plan operation, the local `/plan` fallback spec. TUI-local: `/keys` `/whoami` `/verbose` `/rename` `/btw`, `/agent stream`/`bg`/`cancel` |
| `Staged` | `/model` (bare with picker and `switch <id>`), `/effort`, `/mode`, `/uia-model`, `/uia-worker-model`, `/uia-provider`, `/uia-effort` — the mutating forms of each |
| `DeferredUntilTurnEnd` | `/compact` `/tools` `/new` `/resume` `/sessions` `/clear` `/quit` `/exit` `/export` `/add-workdir` `/retry` `/memory` `/learn` `/context-proposal` `/bug-report`; `/provider test`; `/permissions allow`/`deny`/`remove`; `/matrix` except `show`/`list`; `/research*`, `/explore`, `/analyze`; every write subcommand of `/workbench`, `/kanban`, `/diary`, `/palace`, `/dream` |

There is no `/provider switch` (only `show`/`list`/`test`); a provider
switch runs through `/model` or the model picker.

**Known gap (open at time of writing):** for `/kanban`
(`list`/`show`/`boards`/bare) and `/dream` (`list`/`show`/`status`/bare) the
read forms are meant to be `Immediate` per the design, but
`busy_subcommands` had not yet been set on either operation, so both were
left fully deferred.

**TUI flow** (`route_busy_command` in `harw-tui/src/app.rs`):

1. Local interception first (`local_intercept_for`). Busy-safe intercepts
   apply immediately: overlays and views, model and effort pickers, the UIA
   worker picker, the agent tree, panels, verbose display, system lines,
   rename. Deferred: session selection, clearing the transcript, and
   `Rewrite`.
2. Otherwise `busy_availability_for`: `Immediate`/`Staged` run as their own
   Tokio task with cloned `Arc`s (`BusyJobs`), a 60s time limit; the busy
   `select!` never waits on them, streaming and approvals keep running. The
   result shows as a system line "<command> (during turn)", with `Staged`
   adding "— applies from the next turn". Views open as an overlay or
   panel.
3. Everything else is deferred into `deferred_input` as paste+enter.

While a turn is running, open overlays get the keypresses
(`route_busy_overlay_key`); a command generated from an overlay or picker is
re-classified (`Staged` -> queue it in the controller immediately, never
`apply_to_session` mid-turn). There, Esc first closes the overlay without
aborting the turn. Pending data fetches for open views also run on the busy
path (`spawn_busy_fetches`) so views get populated.

**Visible queue.** Messages sent during a turn (`pending_turns`) show as a
"Waiting for the next turn (N)" block above the composer (at most 3 lines
per message, 10 total). They stay there until delivered on the next turn,
then appear normally in the history; this also holds after a cancel
(§2.6.4). Alt+↑ pulls the most recently queued message back into the
composer.

`/cancel` and `/stop` act on the `JobStore` (background jobs), **not** on
the running turn itself — Ctrl+C and Esc are responsible for that (see
§2.6.4).

### 2.6.4 Ctrl+C and Esc — interrupt without losing the queue

User requirement: Ctrl+C should be a real hard interrupt (model call,
shell/sandbox processes, child agents), not just a cooperatively-checked
signal. A cancel no longer discards already-sent messages.

- **The queue stays:** Ctrl+C and Esc only abort the running turn
  (`interrupt_turn` in `harw-tui/src/app.rs`). Messages and commands sent
  during the turn (`pending_turns`, `deferred_input`) stay queued and are
  delivered right after the cancel at the turn boundary; the status line
  briefly shows "sending queue" (`queue_kept_at`). This also holds when an
  approval or host-permit dialog is open at cancel time. Earlier, Ctrl+C
  used to discard the queue.
- **Esc:** first closes an open popup or overlay and leaves the composer
  text in place; a pending `/btw` side question is the first thing Esc
  cancels. If nothing is open and a turn is running, Esc interrupts it like
  a first Ctrl+C press, but does **not** arm a quit — Esc never closes the
  app.
- **Esc with running children:** while synchronous child agents are
  running, the first Esc only shows "Esc aborts the turn and N running
  agents — press Esc again to confirm"; only the second Esc actually
  aborts. Background agents and already-finished children don't count.
  Without children, Esc aborts immediately as before. `Enter` only queues
  during a turn and never aborts.
- **Model call:** `harw-core/src/turn_loop.rs` races the model call against
  `CancelToken::cancelled()` (`tokio::select!`, `biased`); either a hit
  there **or** an `Err(ModelError::Cancelled)` from the provider (which also
  races against the `CancelToken` from `ModelRequest`) lead into the same
  `cancel_turn(...)` path.
- **Shell/subprocesses:** `harw-tool-shell/src/exec.rs` additionally races
  both `timeout_at` wait points against `cancel.cancelled()`; a hit triggers
  SIGKILL via the existing `terminate()` function and returns
  `Err(ToolsError::Cancelled)`.
- **Child agents:** `ManagedAgentSpawner::register_parent_cancel_token` (in
  `harw-core/src/child_controller.rs`) is wired up —
  `harw-tui/src/app.rs` calls it at the start of every turn (just before
  `drive_turn_animated`, with the same `CancelToken` also passed to
  `TurnControl::with_cancel`), so children admitted afterward inherit
  `cancel.child()`. A registration failure (e.g. because the session itself
  is an admitted child) is not fatal to the turn, only logged
  (`tui.turn.register_parent_cancel_token_failed`).
- **Double-tap idle/busy:** `ChatApp::pending_quit`/`hard_quit_requested`
  (`harw-tui/src/app.rs`) unify the `QuitArm`/`QUIT_HINT_WINDOW` mechanism
  (a 2-second window) across the idle and busy paths: the first Ctrl+C press
  on the busy path cooperatively aborts the running turn and arms
  `pending_quit`; a second press of the same key within the window sets
  `hard_quit_requested`, which `run_loop` checks right after the running
  turn and then exits immediately — the same outcome as `HarwEvent::Quit` on
  the idle path.
- **Fixed 2× Ctrl+C emergency stop (instant, all children):** the double
  press is additionally detected in the blocking input-reader thread
  (`harw-tui/src/input_reader.rs` → `hard_kill::screen_event`), *before* the
  key reaches any dialog, overlay or the async loop, so it fires in every UI
  state and even when the event loop is blocked. The same
  `QUIT_HINT_WINDOW` (2 s) applies; any other key press, or a key *repeat*,
  disarms/doesn't count; only `Press` counts. The reaction
  (`HardKill::trip`, idempotent) is, in this order:
  1. cancel the running turn's `CancelToken` with `CancelReason::Shutdown`
     (inherited by all synchronous child agents) and call
     `ManagedAgentSpawner::cancel_all_background` — every cancel-aware
     `await` wakes at once;
  2. `SIGKILL` the whole process tree below harw
     (`harw-tui/src/process_tree.rs`): descendants are frozen (`SIGSTOP`)
     until the set is stable, then killed; signals go through a `pidfd`
     opened *before* the start-time identity check, so a recycled PID is
     never hit (non-Linux unix: `ps` snapshot + `kill(2)`, no identity
     check). Processes orphaned by the **first** press' cooperative cancel
     (`shell.exec`'s `terminate()` kills only the shell PID) are recorded at
     the first press (`Census`, PID + start time) and killed as extra roots;
  3. wake the event loop, which returns `TuiRunOutcome::Quit` (no queued
     turn is started), saves the session and leaves the TUI; a watchdog
     thread restores the terminal and calls `exit(130)` after 1.5 s if that
     orderly path stalls.

  **Exempt:** the process subtrees of the user's *detachable* background
  jobs (`JobManager::detachable_leader_pids`, i.e. `job.start` jobs). Quitting
  with a double Ctrl+C has always detached running jobs so they survive
  (`jobs_glue::detach_for_exit`); the emergency stop keeps that contract and
  calls `detach_user_jobs`. The processes of job-backed **child agents**
  (`JobManager::start_piped`) are not user jobs and are killed with the tree.
  Limits: a process the unprivileged harw may not signal (e.g. started via
  `sudo`) is counted as `denied`, not killed; a process daemonized *before*
  the first press (its parent already gone) is no longer a descendant.

### 2.6.5 Reasoning-effort defaults (precedence)

User decision: the default reasoning effort is additionally configurable
per provider, per model, and per agent definition; on conflict the
precedence is **provider > model > agent definition > role**
(`reasoning.*` stays the bottom-level fallback).

- The config fields — `ProviderToml.default_reasoning_effort`
  (`harw-config/src/provider_toml.rs`) and
  `ModelToml.default_reasoning_effort` (`harw-config/src/model_toml.rs`) —
  exist with round-trip-tested TOML serialization.
- The four-level resolution function that applies this precedence at spawn
  time exists in two deliberately identical forms (layering rule:
  `harw-core` may not depend on `harw-runtime`):
  `harw_runtime::guard_wiring::resolve_default_reasoning_effort`
  (`harw-runtime/src/guard_wiring.rs`) for the UIA root session (called from
  `harw-runtime/src/assembly.rs`, only when `organizational_role ==
  AgentRoleId::UserInterface` and `spec.reasoning_effort` carries no
  explicit live setting), and a mirrored version in
  `harw-core/src/child_controller.rs`
  (`resolve_child_default_reasoning_effort`) for child agents, served via
  `ChildRegistryFactory::reasoning_effort_defaults_for_role(_task)` —
  `harw-runtime/src/children.rs`'s `RuntimeChildRegistryFactory` overrides
  this method with the `default_reasoning_effort` values actually stored
  for the resolved provider/model ID. `role_effort_weights_from_config`
  remains just the bottom level (role) and serves as the fallback for both
  resolution functions.
  **Existing limitation (not a bug, documented):** a child with no own
  internal model slot (`internal_point_for_role` returns `None`, i.e. no
  matching `internal_models` entry) runs on the inherited parent's main
  model; the `RuntimeChildRegistryFactory` doesn't know that model's
  provider/model ID, so `reasoning_effort_defaults_for_role(_task)` returns
  `(None, None)` for such a child and precedence falls straight through to
  the role level — provider and model defaults only apply to children with
  a resolved internal model slot.

### 2.6.6 Reasoning visibility (UIA)

User requirement: the UIA's reasoning content should be visible, above all
in the UIA session.

**Done, wired end to end:** `harw-core/src/turn_loop.rs` reads
`response.reasoning: Option<OpaqueReasoning>` after every model call. For
Anthropic blocks, `extract_thinking_text` extracts the readable `"thinking"`
text field; `"redacted_thinking"` blocks and encrypted OpenAI reasoning
return no text and deliberately produce **no** `TurnItem::Reasoning` (not an
error). The extracted text is pushed as a `ReasoningItem` into
`session.history_mut()` and emitted as `TurnEvent::ItemAdded {
item: TurnItem::Reasoning(...) }` — **only** for sessions with
`organizational_role == AgentRoleId::UserInterface` (the same criterion as
`is_uia_root_session`); non-UIA sessions still discard reasoning as before.
`harw-tui/src/app.rs` already renders the item via the fully wired
`ReasoningHistoryCell` (dimmed text, `·` prefix). `to_model_messages`
(`history.rs`) skips `TurnItem::Reasoning` on the next model call.

### 2.6.7 Export readability (as of 2026-09-21)

`/export` (§2.1 of the TUI annex) gained several readability fixes without
changing its grammar:

- The start date (`ExportMeta.started_at`) is formatted readably
  (`jiff::Timestamp`, local offset, RFC-3339 style) instead of printed as a
  raw Unix second; the filename still keeps the seconds token. For a
  resumed session (`-r`), the date comes from the session start in the
  session store, not from the TUI start.
- Tool arguments and results are rendered through a new helper
  `render_json_block`: if a string leaf (mainly `value`) is itself JSON, it
  is parsed and printed as nested pretty JSON; otherwise a dedicated
  ` ```text ` block is used with real line breaks instead of a single line
  that could run to tens of thousands of characters.
- Every entry is additionally capped at `ExportOptions.max_chars_per_entry`
  (default 4000 characters), with a `_[truncated: N characters]_` marker,
  character-boundary-safe like the existing `truncate_markdown`; the
  overall export's global `--max-chars` cap still applies on top.
- Headings (`#…` lines) in user and assistant text are demoted by two
  levels (capped at `######`); fenced code blocks are skipped.
- Tool entries (ToolCall/ToolResult/Reasoning) that follow a user message
  now land under the "## harw" heading instead of incorrectly under "## You".
- If copying to the clipboard fails for a plain `/export` (e.g. because harw
  is running in tmux without clipboard access), it instead writes the file
  via the existing `default_export_path` and reports the path. The OSC-52
  output is additionally wrapped in a DCS passthrough envelope
  (`\ePtmux;…\e\\`) in tmux when `TMUX` is set.

---

## 3. `$` prefix — reserved, unassigned for v1

The TUI annex (§3) already reserves `$` without assigning it, listing two
candidates: variable/env interpolation (`$SESSION`) and a budget/cost query
shorthand. This document makes that reservation the final v1 decision,
rather than an open question, for one reason: **grammar squatting**.

Every other prefix in §3 of the TUI annex (`/`, `!`, `!!`, `#`, `@`) has an
unambiguous, singular meaning decided before a single line of parser code
exists. `$` currently has two live candidates that are not obviously
compatible — variable expansion is a *pre-parse* text-substitution concern
(closer to shell behavior, would need to interact with quoting/escaping
rules in §1.1 and §3.1 of the TUI annex before any command even sees its
arguments), while a budget/cost query is an *ordinary command* dressed as a
prefix, more consistent with the existing `/status`-shaped commands than
with a fifth ambient sigil. Picking one now, before either has a full design
pass, risks committing the prefix to whichever candidate got written down
first rather than whichever is actually right — and once a prefix ships and
gets muscle-memory usage, changing its meaning is a breaking change no
version bump inside v1 gets to make casually.

**Decision for v1: `$` is parsed, recognized as a reserved prefix character,
and rejected with a structured error** (a new `CommandError::ReservedPrefix
{ prefix: '$' }` variant, following the same "structured error over silent
fallthrough" convention as `CommandError::UnresolvedRef`) **rather than
falling through to ordinary chat text.** This keeps the door open for either
candidate — or a third one — without any input written today silently
changing meaning later. Candidate designs remain tracked as an open decision
(§6, item 1).

---

## 4. Unified approval model

The TUI annex's `/approve`/`/deny` commands (§2.3) and the channel annex's
Telegram inline-keyboard approval flow (§4.2) are **one model**, not two
that happen to look similar. This section is the single specification both
front-ends implement against; where either annex's prose differs in detail
from what follows, this document's version is normative.

### 4.1 One request/response pair

`harw-protocol::approvals` defines exactly one `ApprovalRequest` and one
`ApprovalResponse` shape, used for every approval-gated action in the
harness: a `/approve`/`/deny` pending work item, a `RiskLevel::High` kanban
claim (knowledge annex §6.4), a channel-invoked `/skills run` (§2.4.1
above), and a `/dream review` per-proposal decision.

```rust
// harw-protocol/src/approvals.rs (shape referenced by both annexes; stated
// here once, normatively, rather than duplicated with drift risk)

/// One pending decision point, addressed at exactly one WorkId (or, for a
/// dream review, one artifact-proposal within a DreamReport).
pub struct ApprovalRequest {
    pub id: ApprovalId,
    pub work_id: WorkId,
    pub summary: String,          // human-readable, rendered verbatim on every front-end
    pub risk: RiskLevel,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub timeout_at: chrono::DateTime<chrono::Utc>,
    pub decisions: &'static [ReviewDecision], // which buttons/subcommands are offered
}

/// The one outcome type every front-end produces, however the human
/// expressed it (`/approve`, `a` keybinding, or a tapped inline-keyboard
/// button).
pub struct ApprovalResponse {
    pub request_id: ApprovalId,
    pub decision: ReviewDecision,   // Approve | Deny { reason: String } | ...
    pub actor: ApprovalActor,       // Tui { session: SessionKey } | Channel { channel: ChannelId, peer: PeerId }
    pub decided_at: chrono::DateTime<chrono::Utc>,
}
```

### 4.2 Front-end mapping — same request, different rendering

| Front-end | Presentation | Decision capture |
|---|---|---|
| TUI, work-graph panel | `/review <WorkId>` opens the diff/plan; `a`/`d` keybindings (§4.2, TUI annex) or `/approve`/`/deny` commands | Keypress or command both construct the same `ApprovalResponse` — the keybinding is sugar for the command, not a separate code path |
| TUI, other surfaces | `/approve <WorkId> [--note=text]` / `/deny <WorkId> --reason=text` | Same |
| Telegram | Inline keyboard rendered from `ApprovalRequest.decisions` (channel annex §4.2) | A `callback_query` maps to `ApprovalResponse` via the in-flight-approvals table keyed by `(chat_id, message_id)` (channel annex §4.2); typed `yes`/`no` fallback text is parsed into the same response type when inline keyboards aren't available |

No front-end constructs an `ApprovalResponse` through a path the other
doesn't also use — the registry dispatch step that consumes an
`ApprovalResponse` cannot tell, and does not need to tell, which front-end
produced it.

### 4.3 One audit rule, not two

Every `ApprovalResponse`, regardless of origin, is journaled with the same
fields: `ApprovalRequest.id`, `WorkId`, the deciding identity (`SessionKey`
for TUI, `SenderRef`+`ChannelId`+`PeerId` for a channel), the decision,
`decided_at`, and — for a channel origin — the raw wire event id (e.g.
Telegram `update_id`) for forensic replay, exactly as the channel annex
already specifies for its own case (§4.2, §5). This document's contribution
is stating that the TUI path writes the *same* audit record shape (with
`ApprovalActor::Tui` in place of `ApprovalActor::Channel`), so an
operator auditing approvals never needs to know or care which front-end a
given decision came through — one audit query, one report format, one
retention policy.

### 4.4 Timeout default

Consistent with the channel annex's Telegram-specific default (§4.2): an
`ApprovalRequest` that receives no `ApprovalResponse` before `timeout_at`
resolves to `ReviewDecision::Deny` on every front-end, not just Telegram.
The TUI additionally reflects this in the work-graph panel (the item moves
out of the pending-review state with a `timed-out-denied` annotation,
mirroring how Telegram edits the message to show the timeout outcome) so
neither front-end's view of an approval can silently disagree with the
harness's actual recorded decision.

---

## 5. Terminology note: two different "operator-only" concepts

Resolving a naming collision found while merging the three documents (see
§7, item 4, for the full list of naming issues resolved): the phrase
"operator-only" appears in two places that are **not the same axis** and
must not be conflated:

- `PermissionTier::Owner` (TUI annex §1.5) is a **command-permission tier** —
  the ceiling on which slash commands a given caller identity may invoke.
  It is strictly ordered against `Observer < Operator < Maintainer < Owner`.
- `VisibilityScope::OperatorOnly` (knowledge annex §7) is a **knowledge-
  artifact visibility scope** — a property of who may read/write a given
  `KnowledgeArtifact` (e.g. dream output defaults to `OperatorOnly`), enforced
  by `harw-policy` independently of which command tier the reading identity
  otherwise holds.

Where this document's plugin/skill requirements describe a command as
"OperatorOnly", that is shorthand for **`PermissionTier::Owner` combined
with `CommandScope::TuiOnly`** (§2.3, §2.4 above) — the highest command tier,
reachable only from the local terminal. It is not `VisibilityScope
::OperatorOnly` and should never be written that way in code or future docs;
the two enums live in different crates (`harw-tui`/`harw-config` vs.
`harw-knowledge`/`harw-policy`) and answer different questions ("can this
identity run this command" vs. "can this identity see this artifact").

---

## 6. Open Decisions register

Every unresolved question raised across the three annexes plus this
document, in one place, numbered, with a status. `open` = genuinely
undecided; `leaning` = a direction is favored but not locked; `blocked-on`
= cannot be resolved until a named dependency exists.

| # | Decision | Status | Source | Notes |
|---|---|---|---|---|
| 1 | `$` prefix final semantic (variable expansion vs. budget/cost query vs. other) | open | TUI annex §7.1 | Prefix reservation itself is now decided (§3, this document); the *assignment* remains open |
| 2 | `/spawn` budget accounting: hard cap vs. soft warning for child-agent token/time budget charged against the parent | blocked-on `harw-job-runtime::Budget` design | TUI annex §7.2 | Same dependency as item 12 below |
| 3 | Command versioning across future channel-native command registration (e.g. re-generating native Telegram bot commands from the internal registry as the registry evolves) | open | TUI annex §7.4 | Channel annex §4.1 already covers per-scope `setMyCommands` generation from a fixed registry snapshot; versioning *across* registry changes is the open part |
| 4 | `/broadcast` fan-out ordering and partial-failure reporting across multiple `ChannelRef`s | blocked-on `harw-catalog` channel registry existing | TUI annex §7.5 | |
| 5 | Chord timeout tunability per-action vs. one global default | open | TUI annex §7.6 | |
| 6 | Pairing-code delivery channel: same unauthenticated DM vs. operator-only side channel | open | Channel annex §Open-1 | Low sensitivity either way per the annex's own framing |
| 7 | `shared_session` topic mode: does the audit log still record the physical `thread_id` even though it isn't part of `SessionKey`? | leaning yes (non-key attribute on the journaled event) | Channel annex §Open-2 | Not yet checked against the session-store journal's actual event schema |
| 8 | Local Bot API server support: fully-trusted extension of the adapter's process vs. its own sandboxing story | open, deferred out of v1 | Channel annex §Open-3 | |
| 9 | Webhook multiplexing: one listener per binding vs. one shared listener by path | leaning toward always-upstream reverse proxy, plain HTTP on loopback | Channel annex §Open-4 | |
| 10 | Cross-channel `TenantId` reuse / session handoff (e.g. continue a Telegram conversation on Slack) | open, flagged for a later `harw-session-store` note | Channel annex §Open-5 | |
| 11 | Palace confidence decay: auto-downgrade `Established` → `Provisional` after inactivity, or surface-only staleness | leaning surface-only | Knowledge annex §Open-1 | Must be decided before `Confidence` enum locks |
| 12 | Workbench retention default for project-scoped surfaces: property of `WorkbenchScope` vs. one global config key | open | Knowledge annex §Open-2 | |
| 13 | Dream job `max_tool_calls` concrete default | blocked-on `harw-job-runtime::Budget` units finalizing | Knowledge annex §Open-3 | Same dependency as item 2 above — both should be resolved in the same pass once `Budget` lands |
| 14 | Kanban storage under truly concurrent OS-process writers (file-locking mechanism, if `harw-job-runtime` ever spawns separate processes rather than threads) | blocked-on `harw-job-runtime` process-vs-thread worker model decision | Knowledge annex §Open-4 | Currently theoretical; becomes real the moment process-based workers ship |
| 15 | Who is authorized to grant `ExplicitlyGranted` palace-visibility widening (the promoting agent, or only an operator) | open, likely folds into `harw-policy`'s approval model | Knowledge annex §Open-5 | |
| 16 | Should `CardState::Blocked{ReviewRequired}` transitions require a linked diary entry rather than a free-text comment | open | Knowledge annex §Open-6 | |
| 17 | `/plugins install` capability-diff format: fixed human-readable template vs. structured + rendered per front-end (only matters if a future channel front-end ever needs to show *any* part of an install flow, which §2.3 currently rules out entirely by making it `TuiOnly`) | open, low priority while install stays TuiOnly | This document, §2.3.1 | Revisit only if a future decision loosens install's `TuiOnly` scope |
| 18 | `CardRef`/`LaneRef`/`BoardRef` ambiguity resolution when the same short name exists on two boards the caller can see | open | This document, §2.1 | The surface form sketch allows a `board:<id>/<lane>` disambiguator but the *default* resolution order (active board first? error on ambiguity?) isn't specified |
| 19 | Whether `/skills run`'s approval requirement should vary by declared skill risk tier (analogous to kanban's `RiskLevel::High` gate) rather than uniformly requiring approval on every channel invocation | open | This document, §2.4.1 | Current rule is intentionally conservative (always require approval from a channel); a risk-tiered skill catalog could relax this for low-risk skills once one exists |

**Register total: 19 open decisions** (5 carried from the TUI annex, 5 from
the channel annex, 6 from the knowledge annex, 3 newly raised while writing
this document).

---

## 7. Naming inconsistencies found and resolved

While merging the three annexes into one grammar, the following naming
issues surfaced. All are resolved as stated here; the annexes themselves are
left unedited (per this task's scope), so this section is the canonical
correction where an annex's wording would otherwise mislead a reader of the
combined system.

1. **`/plugins` and `/skills` had two different definitions.** The TUI
   annex (§2.5) gave both commands one-line, flag-based forms
   (`/plugins [--list | --enable=name | --disable=name]`,
   `/skills [SkillRef] [--search=text] [--install]`). This document's §2.3
   and §2.4 are the superseding, complete subcommand grammars — the annex's
   flag forms should be read as an earlier sketch, not a second valid
   grammar. No code should implement both.
2. **`OperatorOnly` used for two unrelated concepts.** The knowledge annex's
   `VisibilityScope::OperatorOnly` (an artifact-visibility scope) and this
   task's own requirement language ("install/remove/enable/disable =
   OperatorOnly") describe different axes — the latter is resolved to mean
   `PermissionTier::Owner` + `CommandScope::TuiOnly` (§5, this document).
   Future documents should say "Owner-tier, TUI-only" for the command axis
   and reserve "OperatorOnly" exclusively for `VisibilityScope`.
3. **`/workbench` parity was left as a single deferred note.** The TUI
   annex's parity table (§6) marks `/diary`, `/palace`, `/workbench` as
   "Reduced / TUI-only (see per-command note in the knowledge-surface design
   doc)" without the knowledge annex actually specifying a per-command split
   for `/workbench`. §2.2 of this document resolves it at the subcommand
   level (`pin` and the bare panel form are TUI-only; `note` and
   `hypothesis` are `ChannelReduced`) rather than leaving the whole surface
   as one undifferentiated deferral.
4. **`AgentRoleRef` vs. `AgentRef` were never reconciled.** The knowledge
   annex's kanban `Card.assignee` and `Lane::Worker { agent_role:
   AgentRoleRef }` use `AgentRoleRef` (a *class* of agent, e.g. a worker
   role), while the TUI annex's typed-argument table only defines
   `AgentRef` (a specific running agent/session-child). These are genuinely
   different things — a kanban lane is bound to a *role*, not a running
   instance — so both are kept as distinct typed arguments; `AgentRoleRef`
   is added to the ref-resolver system in §2.1 of this document rather than
   overloading `AgentRef` to mean two things depending on context.
5. **`ArtifactId` vs. a missing typed command argument.** The knowledge
   annex defines `ArtifactId` as the stable identifier for every
   `KnowledgeArtifact`, but no command in the TUI annex's typed-reference
   table could address one — `/palace show <node-id>` and `/memory show`
   (knowledge annex §8.2) used a bare "node-id"/"topic-id" string with no
   parser-level type. §2.1 of this document introduces `ArtifactRef` as the
   typed argument every knowledge-surface command uses, closing that gap.
6. **Command inventory totals were inconsistent by construction, not error.**
   The TUI annex's "56 commands" headline (§2, footer) never claimed to
   count the knowledge surfaces or the catalog placeholders at full
   subcommand depth — it explicitly flagged both as reserved-names-only
   (§7.3, TUI annex). §2.5 of this document is the corrected, full count
   (80) once those two areas are worked out; this is a completion of scope
   the TUI annex itself deferred, not a contradiction of it.

---

## 8. Summary of what changed relative to reading the three annexes alone

- The command grammar grew from 56 headline commands to **80**, once
  knowledge surfaces (22 subcommands) and the two catalog domains
  (`/plugins` 7, `/skills` 7) are worked out to full depth instead of
  one-liners/placeholders (§2).
- Six new typed arguments (`BundleRef`, `CardRef`, `LaneRef`, `BoardRef`,
  `AgentRoleRef`, `ArtifactRef`) join the ref-resolver system (§2.1).
- One approval model, not two — TUI keypresses/commands and Telegram inline
  keyboards construct the identical `ApprovalResponse`, audited identically
  (§4).
- `$` stays reserved and unassigned for v1, now for a stated reason
  (grammar squatting) rather than as an unexplained gap (§3).
- Nineteen open decisions from across the three annexes are tracked in one
  register with status (§6), and six naming inconsistencies the merge
  surfaced are resolved with an explicit corrected reading (§7).
