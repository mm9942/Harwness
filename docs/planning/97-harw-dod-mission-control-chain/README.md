---
id: PL-97-HARW-DOD-MISSION-CONTROL-CHAIN
title: "Harw-DoD final suite · tool triggers · TUI Mission Control · execution chain"
status: APPROVED PLAN
date: 2026-10-28
baseline_dev: fcb12b41f5e53c19cc20607a63c98b829452b505
tags: [dod, tools, skills, triggers, tui, mission-control, macros, activity, cgroup, bwrap, landlock]
related:
  - ../94-adaptive-tui-mission-control/README.md
  - ../68-mobile-tui/CONTROL-DECK-V2.md
  - ../71-semantic-activity-patterns/README.md
  - ../95-cli-tools-skills/README.md
  - ../87-tool-gaps/TOOL-DISCOVERABILITY.md
  - ../50-dod-integration/README.md
  - ../20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md
  - ../96-capability-closure/README.md
---

# Harw-DoD final suite · tool triggers · TUI Mission Control — implementation plan

> Written for a **fresh executing session** with no access to the planning
> conversation. Everything needed is in this file: verified baseline facts with
> file paths, the target designs, ordered work packages with acceptance tests,
> and the exact gates. Code is the source of truth; where this plan and the
> tree disagree, re-verify in the tree and note the deviation in the PR.

---

## 0. Operating instructions for the executing session

### 0.1 Branch and base (do this first)
- Designated branch: `ccr-54aa3dfd-o46he2` (push with `git push -u origin ccr-54aa3dfd-o46he2`).
- **The integration branch is `dev`, not `main`.** At planning time `origin/dev`
  (`fcb12b41`) was 582 commits ahead of `origin/main` (`a747cee`), and the branch
  was cut from `main`. Before any work:
  ```bash
  git fetch origin dev main
  git checkout -B ccr-54aa3dfd-o46he2 origin/dev     # branch carries no own commits yet
  ```
  Open the draft PR **against `dev`** (repo convention; see PR #151/#152).
- One branch, one draft PR, **one commit per work package** (WP ids below in the
  subject, e.g. `feat(tui): C2 three status windows (PL-94 W02 U1)`). Push after
  every green WP so the PR is reviewable incrementally.

### 0.2 Repository conventions that gate merges (from CONTRIBUTING.md)
- Rust 1.98.1 pinned (`rust-toolchain.toml`), edition 2024, `unsafe_code = "forbid"`.
- **No `unwrap`/`expect`/`panic!` in production paths.** Tests: prefer
  `fn t() -> Result<..>` with `?` (the TUI uses `type TestResult = Result<(), Box<dyn Error>>`).
- Docs (`docs/**`, READMEs, `///` doc comments on public items) in **English**.
  Inline comments in `harw-tui`, `harw-ops`, `harw-registry-defaults` are German;
  **match the language of the surrounding file**.
- Every new crate must be added to `xtask/arch-policy.toml` `[packages]` with a
  layer (`F` foundation, `I` infra, `C` compiler, `J` jobs, `D` DoD, `A` app) and
  must respect `[rules]` (edges only inward). `cargo run -q -p xtask -- gates`
  fails otherwise. `harw-tui-layout` is layer F (std only, no ratatui).
- Every registered tool needs a `capability_catalog.rs` row
  (`test_every_registered_tool_is_in_the_catalog`), a `tool_permission()` branch
  (`harw-registry-defaults/src/authority.rs`), and an entry in exactly one of
  `AUTO_APPROVED_TOOLS` / `ALWAYS_ASK_TOOLS` (`harw-registry-defaults/src/lib.rs`)
  or it asks every time (fail-closed). Operations declaring
  `model_tool(approval = ...)` are checked by `harw-ops/tests/approval_declaration_gate.rs`.
- Security-touching PRs must state: trust boundary affected, fail-open vs
  fail-closed, what is persisted where (CONTRIBUTING "Security-sensitive changes").
- Planning docs use status labels `DRAFT | APPROVED PLAN | IN IMPLEMENTATION |
  PARTIALLY LANDED | COMPLETED / MOVED TO DESIGN`; never "implemented" inside
  `docs/planning/`. Each landed wave gets a `MIG-*` entry in
  `docs/planning/90-migration-ledger/MIGRATION_LEDGER.md`.

### 0.3 Gate sequence (run per WP; all must be green before the WP commit)
```bash
cargo fmt --all -- --check
cargo clippy --workspace --tests -- -D warnings
cargo nextest run --workspace            # or cargo test --workspace
cargo test --workspace --doc
cargo run -q -p xtask -- gates
make -C dod test                         # DoD crates, -p selection
cargo deny check
python3 -m unittest discover -s scripts/tests -v
```
Fast loops while iterating: `cargo test -p harw-tui-layout`, `cargo test -p harw-tui`,
`cargo test -p harw-ops`, `cargo test -p harw-catalog`, `cargo test -p harw-macros`
(includes trybuild compile-fail tests).

### 0.4 ratatui 0.29.0 facts (verified against docs.rs; the crate pins 0.29.0 + crossterm 0.29)
- `Layout::vertical([..])` / `Layout::horizontal([..])`; `Constraint::{Min, Max,
  Length, Percentage, Ratio(u32,u32), Fill(u16)}`; priority Min > Max > Length >
  Percentage > Ratio > Fill. `.flex(Flex::{Legacy,Start,End,Center,SpaceBetween,SpaceAround})`,
  `.spacing(n)`, `.margin(n)`. `layout.areas::<N>(area)` destructures (`let [a,b] = ..`)
  and **panics if N != constraint count**; `split()` returns `Rc<[Rect]>`.
- `Rect::centered` does **not** exist in 0.29; centre with `Flex::Center`.
  `Rect::contains(Position)` for mouse hit-tests (`Position::new(col,row)`).
  `Rect::is_empty()`; guard 0×0 yourself. `Rect::intersection` exists (already
  used in `agent_monitor::render_dock`).
- `Block::bordered().title(..).title_top(..).title_bottom(..).border_type(BorderType::Rounded)
  .padding(Padding::..)`; `Block::inner(area)`. Titles are not drawn in corners
  unless that edge has no border.
- `List`/`ListState` (`select_next/previous/first/last`, `scroll_padding`,
  `highlight_symbol`, `HighlightSpacing`), `Scrollbar::new(ScrollbarOrientation::VerticalRight)`
  with `ScrollbarState::new(content_len).position(p)` — **content_length must be
  set or it renders blank**. `Tabs::new(titles).select(i)`. `Gauge::ratio` /
  `LineGauge::ratio` **panic outside 0..=1** (clamp first). The TUI today renders
  directly into `Buffer` with `Paragraph`/`Block`/`Clear`/`Scrollbar` only — reuse
  that style for consistency; `List`/`Tabs` are allowed where they simplify.
- `TestBackend::new(w,h)`; `terminal.draw(|f| ..)`; `backend.buffer()`,
  `backend.assert_buffer_lines([...])`. Existing render tests:
  `harw-tui/src/app.rs` (`render_viewport_at_80x40_...`, `narrow_tall_pane_shows_the_agent_dock`),
  `app/live_views_tests.rs`, `app/scroll_routing.rs`.

---

## 1. Context — why this change

Harwness has three big pieces of designed-but-unbuilt work that the owner has
asked to be finished properly, together, and for a different model session to
execute without re-deriving anything:

1. **DoD ("Detect · Orient · Defend", `dod/crates/*`, 34 crates, ~1,400 tests)**
   is real and shipped as system services, but **nothing in `harw` can see or
   drive it**: no `harw dod` command, no operation, no model tool, no TUI
   surface; the findings export is not even written by the shipped unit. The
   owner wants it finalized as the **"Harw-DoD" suite**, controllable through
   `harw`, with its internal functions exposed to the models through properly
   implemented triggers so the models know when and how to use them.
2. **Tool discoverability and triggers (PL-95 / PL-88)**: the deterministic
   skill-trigger engine (`triggers.toml` → `TriggerIndex` → `select()`) exists in
   `harw-catalog` but is **wired to nothing**; the tool index exists but has no
   consumer; the system prompt lists tool names only; four internal tool crates
   (`harw-tool-sys/-cargo/-fsread/-gitread`, PL-93) are built, carry usage
   skills, and are **not registered**.
3. **TUI (PL-68 Control Deck v2, PL-94 Mission Control W00/W01/**W02**, PL-71
   semantic activity + macro metadata)**: the owner wrote the layouts repeatedly;
   only the first portrait dock (R17/R19) and small slices (W02 U0 geometry,
   W01 ranker used for status-line ordering) landed. The three separately
   bordered status windows, the landscape three-column anchors, independent
   focus/scroll per window, the terminal aggregates, job stop via operation,
   inline live cells, and the macro-declared activity metadata are all missing.

4. **Execution chain (PL-20/PL-40, cgroup v2 + Bubblewrap + Landlock)**: the
   job runtime has a delegated-cgroup-v2 backend, a safe Landlock/NNP/caps
   trampoline (`harw-job-exec`) and a Bubblewrap executor, but **bwrap and the
   trampoline are alternatives, never composed**, `shell.exec` bypasses the job
   runtime entirely (no cgroup, no Landlock), and nothing ties harw's
   `harw-job-<attempt>` cgroups to the DoD profile paths and Warden prefixes.
   The owner asked to "properly intertwine Bubblewrap with Landlock" and
   "connect everything".

Outcome: one coherent delivery where `harw` owns DoD (CLI + `/dod` + read-only
model tools + bundled skill with triggers + TUI security surface), the trigger
pipeline is live end-to-end (skills injected on keyword/tool/error/path/command),
every model-driven process runs cgroup ∘ bwrap ∘ Landlock with one honest
enforcement report that DoD can observe and act on, and the TUI renders the
owner-approved Mission Control layouts in portrait and landscape, down to
`harw-tui-layout` geometry, `harw-macros` activity metadata and a shared
activity reducer.

---

## 2. Verified baseline (origin/dev @ fcb12b41, 2026-10-28)

All paths relative to repo root. Line numbers are approximate on dev.

### 2.1 DoD
- Workspace: DoD crates are root-workspace members (PL-60); `dod/Makefile`
  builds with `-p`; CI job `dod` (`.github/workflows/ci.yml:172`) tests them;
  `xtask gates` enforces `edges`, `privileges`, `warden-deps`, `warden-cbuild`, `arch`.
- Facade `dod/crates/harw-dod/src/lib.rs:349-422` re-exports cap/signals/sentinel/
  rules + 9 unprivileged sensors; privileged sensors behind feature `privileged`;
  **no escalate/warden path** (charter §4). Facade has **no consumer** anywhere.
- Binaries: `harw-sentinel` (unprivileged; CLI in `dod/crates/harw-sentinel/src/cli.rs`
  — `--config <PATH>` **exists** at `cli.rs:148-151` (docs claiming otherwise are
  stale); optional `--findings-export <abs path>` at `cli.rs:214-227`),
  `harw-probe-fs`, `harw-probe-bpf`, `harw-warden` (v1 protocol only:
  `dod/crates/harw-warden/src/protocol.rs`, `harw-dod-warden/src/warden.rs:230`).
- Shipped unit `deploy/systemd/harw-sentinel.service:42` runs
  `--log info --home @STATEDIR@ --socket /run/harw/sentinel.sock` — **no
  `--config`, no `--findings-export`**.
- Findings consumers disagree on the path: `harw-runtime/src/security_signals.rs`
  reads `<HARW home>/dod/export/findings.jsonl` (opt-in `[memory] security_signals`,
  default false, registered at `harw-runtime/src/assembly.rs:3374-3384`, context
  label `security.signals`, shown nowhere); `harw-security-hub` (`config.rs:17`)
  reads `/var/lib/harw-dod/export/findings.jsonl` and serves `GET /v1/posture`
  (`server.rs:441`); `harw-infra-client/src/security.rs` has **no posture method**.
- `[dod]` config: `harw-config/src/dod_toml.rs` `DodSection { kill_requires_human
  (forced true), auto_freeze, proof_key_dir, allowed_cgroup_prefixes }`, merged in
  `discovery.rs`, read by **nobody** outside harw-config. Its doc comment calls DoD
  "Definition-of-Done-Sensorik" and references non-existent `harw dod install`.
- `harw-cli`: no DoD command (`harw-cli/src/cli/mod.rs` `Command` enum); only
  `harw install --print-systemd [UNIT]` (embedded units, `harw-install/src/dod_units.rs`)
  and completions pass-through (`DOD_BINARIES`, `harw-cli/src/completions.rs`).
- `harw-operations/src/operation.rs:66` has `OperationDomain::Security` (unused).
  `harw-ops/src/approval.rs:20` invariant S1: harw never calls escalate/warden.
- Agents: `harw-registry-defaults/agents/families/security/security.toml` and roles
  `security-{egress,baseline,structure,endpoint}-triage` exist, **admit no tools**.
- Naming: "Harw-DoD" appears once (`harw-cli/src/cli/completions.rs:5`). Product
  name is "DoD"/"Detect · Orient · Defend"; crates `harw-dod-*`; FHS paths
  `/usr/local/libexec/harw-dod`, `/etc/harw-dod`, `/var/lib/harw-dod`,
  `/var/log/harw-dod`, `/run/harw/{sentinel,warden}.sock`; target `harw-dod.target`.
- Stale docs to fix: `harw-dod/src/lib.rs:259-264` and its Cargo.toml call
  `harw-dod-cgroup` an "empty scaffold" (it is implemented, 1,111 lines);
  facade header says "27 crates" (34); `docs/setup/dod.md:299-332` and
  `dod/README.md` say `--config` is missing.
- Open per PL-96 (`docs/planning/96-capability-closure/README.md`): E12/P0 Warden
  must move to v2 `SignedAuthorization` before any privileged rollout; E05 skill
  trigger selection "not production-wired".

### 2.2 Tools, triggers, discoverability
- Tool contract: `harw_tools::FunctionToolSpec {name, description, parameters, strict}`;
  `ToolProvider { tools(), executor(name), parallel_safe }`
  (`harw-extension-api/src/contributors.rs:91-100`); macro path
  `#[derive(harw_macros::Tool)]` on args + `#[harw_macros::tool(name, description,
  permission, host_from, parallel_safe, state, schema_from)]` on an `async fn`
  (`harw-macros/src/tool.rs`, keys parsed at ~L286-305); `harw_tools::tool_provider!`
  bundles into a provider with `TOOL_NAMES`/`TOOL_PERMISSIONS`.
- Fully wired reference: `harw-tool-process` → dep edge `harw-registry-defaults/Cargo.toml`,
  provider instance in `profile.rs::profile_tool_providers` (Full profile),
  name list `PROCESS_TOOLS`, `tool_permission()` branch (`authority.rs:~684`),
  `ALWAYS_ASK_TOOLS` entry, catalog rows (`capability_catalog.rs`), runner
  feature in `harw-agent-runner/Cargo.toml`, `PROVIDER_FEATURES`.
- Per-turn tool list: `harw-core/src/turn_loop.rs::collect_tools` (~L2535);
  prompt section `## Available tools` is only the name list from the single
  `AgentIdentity::with_tools` call in `profile.rs:~3675`
  (`harw-instructions/src/baseline.rs:365-441`).
- Second route: `#[operation(..., model_tool(readonly, approval = "none"|"always"))]`
  → `Surface::ModelTool` → `ModelToolProvider` (`harw-operations/src/adapter/model_tool.rs`),
  installed for `OperationSurface::AllWithModelTools`
  (`harw-runtime/src/assembly.rs:~3979`). Examples: `harw-ops/src/gateway_ops.rs:382-645`,
  `harw-ops/src/ps.rs:185`. Command surface: `#[operation(name="jobs", summary=..,
  domain="execution", permission="operator", command(path="/jobs",
  visibility="tui_only", busy="immediate"))]` (`harw-ops/src/jobs.rs:125`).
  Registration list: `harw-ops::register_all` (`harw-ops/src/lib.rs:391`).
- Skill triggers: `harw-catalog/src/skill_triggers.rs` (`SkillTriggerSpec`,
  `TriggerKind::{ErrorCode,Command,Tool,Path,Keyword}`, `TriggerEvent`,
  `TriggerIndex`, `parse_triggers_file`), `harw-catalog/src/skill_select.rs`
  (`InjectionBudget`, `InjectionState`, `select(..) -> Selection {injections, dropped}`,
  `catalog_lines(index, task_text, l0_bytes)`, `FRAGMENT_SECTION = "skills.triggered"`).
  **No references outside harw-catalog; no `triggers.toml` exists.**
- Tool index: `harw-registry-defaults/src/tool_index.rs` (`ToolIndex::build(&summary_fn)`,
  `ToolIndexEntry {name, family, provider, crate_name, class, always_available,
  profiles, summary}`) — **no consumer**.
- PL-93 crates `harw-tool-sys`, `-cargo`, `-fsread`, `-gitread`: built, tested,
  each ships `skills/<x>-tools/{skill.toml,instructions.md}` with `tools = [...]`,
  **not registered in any profile and skills not bundled** (PL-93 README status).
  Exception: `cargo.test_one` alone is wired (`profile.rs:65,1222,2943`,
  `ALWAYS_ASK_TOOLS` `lib.rs:457`, parity `capability_catalog.rs:652`,
  `tool-cargo` in `PROVIDER_FEATURES`). `harw-tool-fsread` is also used as a
  library by `harw-plan-bridge/src/cycle_explorer.rs`. Bundled skills come only
  from the hand-written `include_str!` table `harw_home::bundled_files()`
  (`harw-home/src/bundle.rs:102-112`), filtered to `skills/` by
  `harw-catalog/src/skill_index.rs:244-262` — the four tool-crate skill dirs are
  dead files today.
- Newest fully-wired registration example on dev (copy this pattern): PL-90
  `intent_cycle.*` — `INTENT_CYCLE_TOOLS` (`capability_catalog.rs:285`), rows
  `:404-408`, `tool_permission` → `ReadWorkspace` (`authority.rs:727`), approval
  declaration list (`lib.rs:1268`, test), operations in `harw-ops/src/intent_cycle.rs`
  (`start`/`stop` declare `model_tool(approval = "always")`).
- `#[operation(command(...))]` gained `channel_subcommands = "-,list,show"` on
  dev (`harw-macros/src/operation.rs`); copy it on the new `dod` command op.
- Turn loop on dev: a `ToolCallMeter` gate (`harw-core/src/tool_call_meter.rs:142`)
  books and admits every tool call before execution; `collect_tools` at
  `turn_loop.rs:2634`, `find_executor` `:2766`, `assemble_round_request` `:5878`,
  request build `:5910`. Skill injection (B1) hooks into `assemble_round_request`
  **after** the meter has nothing to do with it (the meter governs calls, not context).
- `/skills` operation subcommands on dev: list, show, activate, deactivate,
  proposals, review, accept, reject (`harw-ops/src/skills.rs:685-696`); no model
  tool surface. CLI has a new `Cycle { action }` subcommand; no `Tool`/`Skill`.
- Skills: bundled under `harw-home/assets/skills/*/` (75), loaded by
  `harw-catalog/src/skill_index.rs` (`SkillIndex`, 512 KiB cap); pull tools
  `skills.search`/`skills.load` (`harw-registry-defaults/src/skill_tools.rs`);
  bound skills become instruction fragments in `harw-runtime/src/assembly.rs:~2264`.
  Per-turn context goes through `gather_context` in `assemble_round_request`
  (`harw-core`), yielding `harw_context::Fragment`s with `Stability` and section.
- `harw-context/src/sources.rs:56` is the source table (budget shares); add new
  sections there.

### 2.3 TUI (harw-tui ≈ 90k lines, 1,177 tests; harw-tui-layout 4 files)
- Loop/render: `harw-tui/src/app.rs` — `run_loop` (~L4738, `tokio::select!`),
  `render_viewport` (~L9900-10700): overlays → dialog heights → `queue_lines` →
  `status_rows_for` → `panes::classify_screen` → **portrait branch** (dock +
  `portrait_live_strip` + chat) or **wide/summary branch** (`panes::split`, right
  column agents 60 % / workbench 40 %, explorer left) → status → history →
  popups → dialogs → composer. `FrameRegions {agents, dialog, plan}` for mouse
  routing (`app/scroll_routing.rs`).
- Geometry: `harw-tui-layout/src/placement.rs` `Placement::{WideSide (cols>=100),
  PortraitDock{split} (40<=cols<100 && rows>=28; dock = body/3 clamped 8..15),
  Summary, Compact}`, `classify(LayoutInput{cols,rows,status_rows,composer_rows})`;
  `dock.rs` `split_dock` (`PHONE_DOCK_SPLIT_MIN_COLS = 68`) → `DockSplit::{Combined,
  Split{agents,jobs}}`; `mini_live.rs` (W02 U0) `chat_first(ChatFirstInput{area,
  live_body_lines_wanted, min_chat_rows}) -> ChatFirstLayout{cards: Option<DockSplit>,
  finished: Option<Rect>, live: Option<Rect>, live_mode: LiveMode{Hidden,Header,Body{lines}},
  chat}` with `CARDS_ROWS = 4`, `MINI_LIVE_MAX_BODY_LINES = 4`, `MINI_LIVE_MIN_CHAT_ROWS = 8`.
- `harw-tui/src/panes.rs`: `classify_screen`, `DockAreas::{Combined, Split{agents,jobs}}`,
  `split_dock_areas`, `PortraitLive {live, body_lines, chat}`,
  `portrait_live_strip(below_dock, body_lines_wanted)` (W02 U1 partial: strip
  content is the two **recency slots** (`recency_view::slot_lines`, h7), not the
  selected agent's trace), `PaneFocus::{Chat,Explorer,Agents,Workbench}`,
  `PanelState {agents_visible, explorer_visible, workbench_visible, maximized, focus}`,
  `split(area,&PanelState) -> PaneAreas{explorer,chat,agents,workbench}`.
  **Note: `chat_first.cards/finished` are computed but the portrait branch still
  renders the old `render_dock` into the classify dock; there is no Finished/Failed
  card yet.**
- Agents/jobs data: `harw-tui/src/agent_monitor.rs` `AgentMonitor {agents:
  BTreeMap<String,AgentLive>, order, selected, finished_expanded, panel_scroll,
  follow_selection, panel_height, jobs: Vec<JobRow>}`, `AgentLive` (phase
  `AgentPhase::{Admitted,Thinking,Tool,Waiting,Done,Failed,Cancelled}`, task, tool,
  usage, `reasoning_preview`, bounded `TraceEntry` list, failure, parent, role),
  `panel_entries() -> Vec<PanelEntry::{Agent{depth,live},Finished,Job{row},JobsFinished}>`
  (one shared selection + scroll for the whole list), renderers `render_dock`
  (~L2372), `render_agents_only_panel`, `render_jobs_only_panel`,
  `render_agents_panel`, `render_collapsed_summary`, `render_agent_detail`.
  `harw-tui/src/jobs_panel.rs` `JobRow {id,name,state,progress,runtime,active,failed,reason}`
  from `harw_tool_job::JobStatus` (`app/jobs_glue.rs` refresh); `render_job_detail`.
  `harw-tool-job/src/model.rs` `JobMeta { origin_call_id, origin_tool, owner_agent, .. }`
  — **no typed child-session provenance**.
- Live/inline: `harw-tui/src/child_stream.rs` `ChildStreamBlock`/`ChildStreamRegistry`
  (bounded, sanitized live child output; attached once after `ChildSpawned` in
  `app/child_stream_glue.rs`; `/agent stream <orchestrators|all|none>`, Ctrl+O);
  `harw-tui/src/live_ranker.rs` `LiveActivityRanker` (`rank` ~L138, `pin` ~L125),
  `ActivityTier::{NeedsOperator,Milestone,Active,Quiet,Terminal}`, `LiveCandidate`,
  `RankerConfig { coalesce_ms: 750, lead_dwell_ms: 3000, recency_halflife_ms: 10_000,
  terminal_display_ms: 5000 }` — used only by `app/live_order_glue.rs::child_info_display_order`
  (called at `app.rs:~10504`) to order status-line child segments (W01 M2b); the
  implicit "pin" is "selected child first while `PaneFocus::Agents`". `AgentLive`
  has `last_meaningful: Instant`. `recency.rs`/`recency_view.rs` hold slot1/slot2
  of the last `parent.message` agent messages (with `role`).
- W02 state on dev, precisely: U0 done (`mini_live.rs`, 8 tests incl.
  `table_of_terminal_sizes_partitions_the_area` over 46×78, 48×65, 70×90, 39×60,
  48×19, 80×40, 120×42, 160×45, 1×1, 0×0); U1 partial (`render_live_strip`
  `app.rs:~9877`, label `" Live "`, drawn between dock and chat; **`layout.finished`
  is never rendered — no Finished/Failed card exists**); U2 not per spec (strip =
  recency slots, no selected agent, no tool `name · state · duration`); U3 absent
  (`FrameRegions` still `{agents, dialog, plan}`); U4 partial (tests
  `portrait_dock_shows_the_recency_slots_in_the_mini_live_strip_at_46x78`,
  `portrait_dock_strip_keeps_message_room_with_real_session_id_at_40_cols`,
  `portrait_dock_without_recency_has_no_mini_live_strip`,
  `portrait_dock_collapses_the_strip_to_its_header_when_space_is_short`,
  `panes::portrait_live_strip_follows_chat_first_at_46x78`,
  `panes::portrait_live_strip_collapses_body_then_header_before_the_chat`; no
  render tests at 48×65/70×90). W02 doc still says DESIGN ONLY.
- History: `harw-tui/src/history_cell.rs` — `ToolCell` (shared, updated in place
  by `ToolCallId`), `ToolGroupCell` (hard-coded consecutive read-only names in
  `ToolGroupCell::accepts` at `history_cell.rs:~3038`: `"fs.read" | "fs.search" |
  "fs.grep" | "fs.list" | "fs.glob" | "doc.read_pdf"`; tally at ~L3053: read =
  `fs.read|doc.read_pdf`, search = `fs.search|fs.grep`, list = `fs.list|fs.glob`), `SubAgentCell`,
  `ReasoningHistoryCell`, `PlanGraphCell`, `GoalCell`; `chat_scroll.rs` is
  line-count based (no cell identity anchoring).
- Keys: `harw-tui/src/keybindings.rs` `KeyAction` (19 variants: ToggleExplorer F2,
  ToggleAgents F3, CycleFocus F4, MaximizePanel F11, FocusExplorer Ctrl+E,
  ToggleToolCells Ctrl+O, EndHostMode Ctrl+H, DeleteLine Ctrl+K, InsertNewline
  Ctrl+J, CyclePermissionMode Shift+Tab, ToggleWorkbench F5, OpenKanban F6,
  OpenModePicker F7, OpenModels F8, ShowHelp F1, OpenMatrix F9, ScrollPanelUp/Down
  Ctrl+↑/↓, RedrawScreen Ctrl+L) with `KeyAction::ALL: [Self; 19]`, flat TOML
  `[tui].keybindings_file`. Agent-list keys in `app.rs::handle_panel_key`
  (`j/s/↓`, `k/w/↑`, PgUp/PgDn, Home/End, `f`, `c`, Enter).
- Commands: `#[operation]` → `OperationMeta` → `CommandAdapter` → `CommandRegistry`
  (`harw-tui/src/registry.rs`, `command_exec.rs::execute_command_as`) — panel keys
  must never bypass this (CONTROL-DECK-V2 §9).
- Overlays: `Overlay::{AgentTree, SessionPicker, ExportChoice, ModelSwitch,
  EffortChoice, View(Box<dyn OverlayView>), JobsQuitChoice}`; `OverlayView` impls:
  `HelpOverlay`, `ModePicker`, `ModelRolesView`, `KanbanBoard`, `KnowledgeBrowser`,
  `MatrixView` (`overlay_view.rs` trait: render/handle key/scroll).
- Launch: `harw` / `harw chat` → `harw-cli/src/chat.rs::run_chat` →
  `harw_tui::runtime_root::run_tui(Arc<RuntimeAssembly>, TuiRunOptions)`;
  everything in-process (AgentEventHub + JobManager + OperationRegistry).
- Macros: `harw-macros/src/lib.rs` has 18 proc macros; `#[tool]` has **no
  `activity(...)`**; `permission_request!` (planned in
  `docs/design/dod-system-operations-authority.md` §6.3) does not exist.
- Approved UX precedence: **W02 (2026-10-09) supersedes the PL-94 Progress
  Agent / portrait progress bar / second output pane.** Three separately bordered
  status windows + a small read-only LIVE projection of the *selected existing
  agent* + Chat largest; no new model call, no observer job, no todo synthesis.
  Landscape keeps PL-94 anchors (Chat left; Finished/Failed top-centre; LIVE
  below it; Agents above Jobs far right). W20 (Control Deck v2) supplies the
  **terminal aggregates by typed category + managed-job stop through `/jobs stop`**;
  W01 supplies **inline activity-ranked live cells** (ranker exists, cell
  projection does not).

### 2.3b Execution chain: cgroup v2, Bubblewrap, Landlock, DoD (dev)
- **Job runtime (PL-20 phases 1–7 landed, MIG-003…006):** `harw-job-runtime/src/coordinator/linux.rs`
  `LinuxExecutor::new(LinuxExecutorOptions { cgroup_root: Option<PathBuf>, sandbox:
  LinuxSandboxBackend, exit_status_dir, .. })`; backends `LinuxSandboxBackend::{None?,
  LandlockTrampoline { trampoline: PathBuf, plan_dir, .. }, Bwrap(BwrapExecutor)}`
  (~L111–130). Start order (§24 of PL-20, implemented): create job cgroup
  `harw-job-<attempt>` beneath the **delegated** root (`CgroupV2Fs::open(root)`,
  `harw-job-linux/src/cgroup.rs`; limits from typed `JobResources`) → plan sandbox
  → requirement check → spawn as process-group leader → `pidfd_open` → cgroup
  attach (`LinuxJobGroup::spawn_in_cgroup`) → recovery identity → Tokio supervise.
  Recovery reopens the cgroup by persisted path and kills the whole cgroup.
- **Trampoline `harw-job-exec`** (`harw-job-exec/src/{trampoline,plan,parent,report}.rs`):
  safe re-exec helper; reads a `0600` `ExecPlanV1` file (`CgroupJoin`, `RlimitSet`,
  `SandboxPolicy`, `SandboxRequirement`), then cgroup `attach_self` → `setrlimit`
  → `NO_NEW_PRIVS` → capability drop → Landlock `restrict_self`
  (`harw-job-linux/src/sandbox.rs::apply_to_current_process`, feature
  `linux-sandbox`, ABI target V5/Linux 6.10, honest per-dimension
  `SandboxReport`; network via Landlock is `Partial` at best) → writes report
  → exits `126` if the requirement is unmet → `exec`. Used by
  `coordinator/linux.rs:~870` (`TrampolineCommand::new(trampoline, &plan, plan_dir)`).
- **Bubblewrap executor** `harw-job-executor-bwrap` (`executor.rs`): maps
  `SandboxPolicy` → `harw_sandbox::BwrapLauncher` plan (`--unshare-all
  --unshare-net` always; proxy-only egress via `harw-netns-relay`; `/sys`,`/run`
  withheld; `/usr,/bin,/lib,/lib64` ro; workspace bind). Crate docs state the
  gap verbatim: *"Future: trampoline inside the sandbox — rlimits and a Landlock
  layer inside the namespace can later be added by running the harw-job-exec
  trampoline as the sandboxed command (`bwrap … -- harw-job-exec … -- <program>`).
  Until then `resource_limits` is predicted as `NotEnforced`"*; mount namespaces
  cannot distinguish read from execute, Landlock can. **Bwrap and Landlock are
  therefore alternatives today, never composed.** Between spawn and cgroup
  attach the bwrap child runs a few µs outside the cgroup (trampoline path
  joins itself).
- **`shell.exec`** (`harw-tool-shell/src/exec.rs:~725-834`): builds the bwrap
  argv itself (`BwrapLauncher::discover()` + `plan`), wraps it as
  `prlimit … -- bwrap …` (`limits.rs`; `ShellLimits` as 2 GiB / cpu 60 s /
  fsize 256 MiB / nofile 256 / nproc 4096 / tmpfs 256 MiB, not in TOML) and
  submits that argv to the job port as **`CommandSandbox::Host`** — so bwrap
  runs as an *unsandboxed host job*: no cgroup, no Landlock, no
  `SandboxReport`. `job.start` goes the same way through `ShellJobLauncher`
  (`harw-tool-job/src/manager.rs:~350, ~815 start_on_runtime`). **Child agents**
  (`harw-agent-runner/src/job_child_backend.rs:263-285`, `JobManager::start_piped`
  with `executed_on_host: true`) are plain host processes: no bwrap, no
  Landlock, no cgroup. `container.run` goes to Podman through the port, not the
  OCI executor. `harw verify-sandbox` (`harw-cli/src/verify_sandbox.rs`) builds
  the executor from `HARW_VERIFY_CGROUP_ROOT` and picks trampoline-or-bwrap
  (`~L149-180`). `job_resources` (`coordinator/linux.rs:~349`) maps only
  `memory_max`, `cpu_weight`, `pids_max` from `JobSpec` (`cpu_max`, `io`,
  `memory_high` never set). No seccomp anywhere (PL-20 §4.3 defers it).
- **Production wiring gap (verified):** the real host port
  `harw_command::JobCommandPort::host` (`harw-command/src/port.rs:~298-315`,
  called from `harw-runtime/src/host_escalation_wiring.rs:238`) builds
  `LinuxExecutor::new(LinuxExecutorOptions { new_session: true, ..Default })`
  — i.e. **`sandbox: LinuxSandboxBackend::None` and `cgroup_root: None`**. In
  production, managed jobs, `!` shell and child agents today get pidfd +
  process group only: no cgroup, no Landlock, no bwrap. Only
  `harw verify-sandbox` (`harw-cli`, feature `bwrap` enabled there) ever
  instantiates a backend. `LinuxSandboxBackend` is `#[non_exhaustive]` with
  `None | LandlockTrampoline { trampoline, plan_dir } | Bwrap(..) (feature "bwrap")`.
  The trampoline binary is located next to the running `harw` executable
  (`verify_sandbox.rs:~197`, `TRAMPOLINE_NAME = "harw-job-exec"`); the root
  `Makefile` `build`/`install` targets do **not** build or install
  `harw-job-exec` (only `harw`, `killer`, `harw-agent-runner`).
- **Findings export schema** (`harw-sentinel/src/export.rs::encode_line`):
  `{finding_id, host, rule_id, severity, summary, observed_at}` — **no cgroup/
  scope field**, so findings cannot be correlated to jobs today.
  `LinuxRecoveryIdentity.cgroup_path: Option<String>` (`harw-job-linux/src/recovery.rs:~42`)
  already persists a job's cgroup path. Sentinel has `--cgroup-root`
  (`harw-sentinel/src/cli.rs:~182`, default `/sys/fs/cgroup`).
  `deploy/systemd/harw-control.service` has no `Delegate=`.
- **Host probe:** `harw-job-runtime/src/host.rs` `HostReport::probe_with_cgroup_root`
  → `cgroup_v2_delegated`, landlock ABI, userns.
- **Config (verified):** `[sandbox]` (`harw-config/src/harness_config.rs:~480`:
  `cargo { mode, cargo_bin, rustup_home, cargo_home }`, `tmux { mode, socket_path }`),
  `[jobs] max_running` (default 16), `[shell] max_timeout_secs`; **no config key
  for `cgroup_root`, the sandbox backend, Landlock or bwrap** (env only:
  `HARW_VERIFY_CGROUP_ROOT`, tests `HARW_TEST_CGROUP_ROOT`). No `[execution]`.
- **DoD side:** profiles (`dod/crates/harw-dod-config/src/lib.rs:205-617`) select
  `scope = "cgroups"` with absolute `cgroup_paths = [CgroupPath]` + explicit
  `include_descendants` (`CgroupPath::is_same_or_ancestor_of`, no bare prefix
  matching), or `scope = "host"`; sensors `exec`, `tcp-connect`. `harw-dod-cgroup`
  reports `memory.current/max`, `pids.current`, `cpu.stat usage_usec` for the
  **direct children** of its scope root only, max 16 cgroups (`sensor.rs:82`).
  **BPF scope is static:** `harw-dod-bpf/src/profile.rs:24-83` expands
  descendants **once at attach time** into the `SCOPE_CGROUP_IDS` map (kernel
  side `bpf_get_current_cgroup_id` lookup, `dod/bpf/include/harw_dod_wire_v1.h:117-120`)
  — a `harw-job-<attempt>` cgroup created after the probe started is **not
  observed** until a restart. **No attribution:** the wire header carries
  `cgroup_id`, but `harw-probe-bpf/src/collect.rs:329-332` emits
  `Actor.cgroup = None` for exec events. Warden executor
  (`dod/crates/harw-dod-warden/src/executor.rs:194-252`, `CgroupV2Executor`)
  writes `<root>/<relative id>/cgroup.freeze|cgroup.kill` (e.g. `harw.slice/job-1`);
  `harw-warden/src/isolation.rs` has a real nft isolator (`socket cgroupv2 level N`).
  **Two prefix allowlists with mismatched formats and no verified consumer:**
  harw `[dod] allowed_cgroup_prefixes` (absolute, e.g. `/sys/fs/cgroup/harw.slice/`)
  vs `ProofPolicy` in `harw-dod-warden-proto/src/signed.rs:319-382` (relative,
  v2 proof path — unwired while the Warden verifies v1). DoD daemons sandbox
  themselves with Landlock ABI V1 (`harw-sentinel/src/sandbox.rs:216`,
  `harw-probe-bpf/src/landlock.rs:155`, `harw-warden/src/landlock.rs:212`).
  Nothing today ties `harw-job-<attempt>` names, the delegated root, the DoD
  profile paths and the Warden prefixes together.

### 2.4 Branch salvage (unmerged work on other branches)
See §6 — port candidates are listed there with their WP.

## 3. Target design

### 3.1 Harw-DoD suite

**Naming decision.** "Harw-DoD" becomes the *product/suite name* in all
human-facing text (README, `docs/setup/dod.md` → title "Harw-DoD (Detect ·
Orient · Defend)", `dod/README.md`, CLI `about`, systemd `Description=` lines in
`deploy/systemd/harw-*.service|socket|target`, `deploy/sysusers.d/harw.conf`
comments, planning READMEs). **Crate names, binary names, FHS paths, socket
paths, groups, config keys, unit file names stay unchanged** (charter §2 fixes
`harw-dod`; renaming 34 crates/paths would break every install, the TCB gates
and `harw-install` parity tests for zero functional gain). Fix the stale facts
listed in §2.1 in the same commit.

**Export pipeline (prerequisite for everything harw reads).**
- `deploy/systemd/harw-sentinel.service`: add `--config /etc/harw-dod/config.toml`
  and `--findings-export /var/lib/harw-dod/export/findings.jsonl`; the unit
  already has `ReadWritePaths=` for `/var/lib/harw-dod` (verify; add
  `/var/lib/harw-dod/export` to `deploy/tmpfiles.d/harw.conf` as `d 0750
  harw-sentinel harw-dod-readers`). New group `harw-dod-readers` in
  `deploy/sysusers.d/harw.conf` (members: none by default; the operator is added
  by `harw dod enable --grant-read <user>`; documented). `harw-install` parity
  tests must be updated (they fail on any drift — that is expected; update fixtures).
- Sentinel: add `--health-export <abs path>` (default when `--findings-export` is
  set: sibling `health.json`), written atomically (tmp + rename) every poll cycle
  with `{ schema_version: 1, written_at, config_digest, profile, sensors: [{id,
  health: SensorHealth-as-string, degrade_reason?, last_ok_at?, readings_total,
  errors_total}], ipc: {probes_connected, events_total, dropped_total} }`.
  Source: the existing `SensorHealth`/`DegradeReason` in `harw-dod-sentinel/src/health.rs`
  and the IPC counters in `harw-sentinel/src/ipc.rs`. Keep it in the unprivileged
  binary; no new capabilities.
- One shared resolver for "where is the export": new small crate
  **`harw-dod-client`** (layer `I`; deps: `harw-dod-config`, `serde`, `serde_json`,
  `time`, `harw-types`; **no** warden/escalate/sentinel-binary deps; add to
  `arch-policy.toml`). Exposes:
  ```rust
  pub struct ExportPaths { pub findings: PathBuf, pub health: PathBuf }
  pub fn resolve_export_paths(override_: Option<&Path>, harw_home: &Path) -> ExportPaths
      // 1. explicit `[dod] export_dir`, 2. /var/lib/harw-dod/export if readable, 3. <home>/dod/export
  pub fn read_findings_tail(path, max_bytes: u64 /*256 KiB*/, now) -> Result<Vec<FindingLine>, DodClientError>
  pub fn read_health(path) -> Result<Option<HealthReport>, DodClientError>
  pub fn summarize(lines: &[FindingLine], window: Duration) -> FindingsSummary // per rule/severity counts
  pub fn read_system_config(path: Option<&Path>) -> Result<ConfigReport, DodClientError> // via harw_dod_config: valid(profile)/missing/invalid(reason); never follows symlinks
  pub fn unit_states() -> Result<Vec<UnitState>, DodClientError> // `systemctl show -p ActiveState,SubState,UnitFileState <unit>` for the 6 units; absent systemctl => NotAvailable
  pub struct DodStatus { install, config, units, warden: WardenState{Disabled,Enabled,Unknown}, health: Option<HealthReport>, findings: FindingsSummary, export_paths }
  pub fn status(ctx: &DodClientContext) -> DodStatus
  ```
  `security_signals.rs` switches to `resolve_export_paths` (behaviour unchanged
  when the override is absent and the system path is unreadable).
- `harw-config` `[dod]`: add `export_dir: Option<PathBuf>` (profile scope,
  untrusted layers may **not** set it — same rule as `proof_key_dir`) and
  `findings_window_hours: u32` (default 24). Fix the module doc ("Definition-of-
  Done" → "Detect · Orient · Defend"; drop the `harw dod install` claim).

**CLI `harw dod` (noun-verb, `--json` via the existing `Printer`).**
In `harw-cli/src/cli/dod.rs` + `harw-cli/src/dod_cmd.rs`:
```
harw dod status                 # DodStatus (text table / --json)
harw dod doctor                 # read-only prerequisites (wraps the checks of dod/scripts/doctor.sh in Rust: tools, BTF, units installed, config present/valid, export readable)
harw dod findings [-n 50] [--severity high|critical|..] [--rule <id>] [--since 24h]
harw dod sensors                # health.json table
harw dod config check [--config PATH]
harw dod config suggest [--drop-in] # profile stanza + [dod] prefixes for harw's delegated cgroup root (E3); --drop-in prints the sentinel unit drop-in
harw dod enable|disable|restart # systemctl on harw-dod.target (+ sentinel/probe-bpf); refuses without root (euid 0) and without a valid active_profile
harw dod warden status|enable|disable   # enable = create /etc/harw-dod/warden.enable + enable socket; prints the kill_requires_human reminder; root only
harw dod install --print-systemd [UNIT] # move/alias of today's `harw install --print-systemd` (keep the old spelling as hidden alias)
```
Exit codes: 0 ok, 1 failure, 2 usage, 3 refused by policy/not root (PL-95 §3.4).
Mutating subcommands never run cargo/make; they call `systemctl` and write the
one flag file, matching `dod/Makefile` `enable`/`enable-warden` semantics (read
them first and keep the behaviour identical). `harw completions --all-binaries`
already covers the DoD binaries; add the new subcommand to the clap tree test.

**Operations (`harw-ops/src/dod.rs`, registered in `register_all`).**
- `DodService` (Arc, in the `ServiceMap`) wrapping `harw-dod-client` with a
  short cache (5 s) so repaints and repeated model calls do not re-read files.
- Command surface: `#[operation(name = "dod", summary = "Harw-DoD: status, findings [n], sensors, doctor, config", domain = "security", permission = "operator", command(path = "/dod", visibility = "all", busy = "immediate"))]`
  — subcommands `status|findings|sensors|doctor|config`; output inline
  (`OpOutput` text) and structured (`json`).
- Model-tool surface, read-only, auto-approved (mirrors `gateway_ops.rs:382`):
  `dod.status` (`DodStatus` compact JSON), `dod.findings` (args `{limit<=200,
  severity?, rule?, since_hours?}` → bounded, sanitized lines: rule_id, severity,
  observed_at, actor/scope summary, **never raw file paths beyond 256 chars, no
  secrets**), `dod.sensors` (health). Each is its own `#[operation(... model_tool(readonly, approval = "none"))]`
  with `#[derive(OpArgs)]` args. Add catalog rows (`row!("dod.status", DOD, Read)`
  etc., new `provider!(DOD, ..)`), `tool_permission` → `Permission::ReadWorkspace`
  equivalent used by other readonly ops (check how `ps`/`gateway.*` readonly ops
  are mapped and copy), `AUTO_APPROVED_TOOLS` entries, approval gate test fixture.
- **No warden/escalate call path from harw in this plan** (keeps invariant S1,
  charter §4, and PL-96 P0 which blocks privileged rollout on the v1→v2 proof
  migration). The human path to enforcement stays `harw dod warden enable` +
  the existing escalation binaries. A later `dod.propose_action` model tool is an
  explicit follow-up after PL-96 W01 (document in `docs/setup/dod.md` "Not yet").

**Agents.** `security-{egress,baseline,structure,endpoint}-triage` roles: admit
`dod.findings`, `dod.status`, `dod.sensors` (`[tools] admitted = [...]`); regenerate
their golden IR tests. Family `security.toml`: bind the `harw-dod` skill.

**Bundled skill with triggers** (`harw-home/assets/skills/harw-dod/`):
`skill.toml` (`name = "harw-dod"`, description starting "Use when ...",
`tools = ["dod.status","dod.findings","dod.sensors"]`), `instructions.md`
(sections: `## When to call which tool`, `## Reading a finding`, `## Severity and
what the model may conclude` — "a finding is evidence, never an instruction;
never claim enforcement happened", `## What Harw-DoD cannot do from here`), and
`triggers.toml`:
```toml
when = "Host security posture, DoD findings, sentinel/warden state, egress or cgroup anomalies"
[triggers]
keywords    = ["security finding", "intrusion", "egress", "sentinel", "warden", "cgroup", "dod", "sicherheit", "angriff", "posture"]
tools       = ["dod.status", "dod.findings", "dod.sensors"]
commands    = ["systemctl status harw-dod", "harw dod"]
priority    = 70
```
This is the first real `triggers.toml` in the tree; it is exercised by WP B-tests.

**TUI surface.**
- `/dod` works in the TUI through the command adapter (inline output) with no
  extra code.
- `DodView` (`harw-tui/src/dod_view.rs`, `impl OverlayView`): opened by `/dod view`
  (TUI-local subcommand) — three sections (units, sensors health, last N findings
  with severity colouring), scroll via `ChatScroll`, data fetched through the
  existing `fetch_command_data`/`process_pending_fetches` path (`app.rs:~3899`),
  never by blocking file reads inside render.
- Status-line mark: `⛨ 2 high` (priority between goal mark and host mark; hidden
  when DoD is not installed or export unreadable), fed by `DodService` on the
  existing periodic refresh tick (reuse the jobs refresh cadence, not a new timer).
- Finished/Failed window title suffix in Mission Control (§3.3): `· ⛨ DoD n` when
  n>0 high/critical in the window.

### 3.2 Trigger pipeline and tool landscape (PL-95 S3–S5, PL-88, PL-93)

**S3 — inject triggered skill sections per turn (harw-core).**
- New `harw-core/src/skill_injection.rs`: `SkillInjector { index: Arc<TriggerIndex>,
  state: InjectionState, budget: InjectionBudget, enabled: bool }` stored per
  session (next to the other per-session turn state). Event feed inside
  `assemble_round_request` from the history items since the last round:
  user text → `TriggerEvent::Task`; tool results → `ErrorCode` scan; first use of
  a tool name this session → `Tool`; `fs.*`/`doc.*` args → `Path`; `shell.exec`/
  `process.*` commands → `Command`. Call `harw_catalog::skill_select::select`,
  map each `Injection` to a `harw_context::Fragment` (section `skills.triggered`,
  `Stability::Fresh`, trust `Instruction` for bundled/trusted-layer skills,
  `Evidence` for imported) placed after the stable prefix. Record an
  `injection` event in the session store (skill, section, kind, key, bytes, turn).
  On compaction, reset dedupe for dropped items.
- Budget: new section in `harw-context/src/sources.rs` (`skills.triggered`, share
  ≈3 % of window clamped 1–4 KiB/turn), session cap 16 KiB, L0 catalog 1.5 KiB
  (replace today's one-line `skill_catalog_hint` with `catalog_lines(...)`).
- Config `[skills]` (new section in `harw-config`): `triggered_injection = true`,
  `inject_imported = false`, `per_turn_bytes`, `per_session_bytes` (profile scope).

**S4 — tool → skill link.** `ToolIndexEntry` gains `skill: Option<&'static str>`;
the registry records tool→skill from (a) `tool_provider!` metadata (new optional
`skill = "sys-tools"` key on `#[tool]`, see §3.5) and (b) `skill.toml` `tools = [...]`
of bundled skills (`SkillIndex` builds the reverse map). First use of a tool
whose skill has no `triggers.toml` still fires a synthetic `Tool` trigger for its
linked skill ("tools bring their skill").

**PL-88 — discoverability.** (1) model tool `tool.index` (readonly operation,
`harw-ops/src/tool_index_op.rs`, args `{family?}`, cheap table output) built from
`ToolIndex::build` + the real `ToolSpec` summaries; (2) `harw tool list [family]`
and `harw tool describe <name>` (CLI, `--json`); (3) system prompt block
`## Tool families` (families with counts + "call `tool.index` for details"),
generated in `AgentIdentity` from the index so the prompt stays cache-stable
across turns; the existing `## Available tools` name list stays. (4) Registry
lint test: every registered tool has a description ≥ 60 chars that starts with
"Use when"/"Returns"/verb (one documented regex) and every arg field has a
description — fails the gate.

**S2 — `harw skill` and `/skills` additions.** `harw skill list|show <name>|
preview "<task text>" [--tool X] [--error E0505] [--path p] [--command c]
[--budget N]|validate <dir>`; `preview` prints the `Selection` (injections +
dropped with reasons) with no model call. `/skills preview ...` in the TUI goes
through the same operation (`harw-ops/src/skills.rs` gains `preview`/`validate`).

**PL-93 — register the internal tool crates.** Wire `harw-tool-sys`,
`harw-tool-cargo`, `harw-tool-fsread`, `harw-tool-gitread` exactly like
`harw-tool-process` (dep edge, provider instance in `profile_tool_providers`,
name lists, `tool_permission`, catalog rows, approval lists — all read-only
tools auto-approved; `cargo.*` build/test tools `execute_process` + auto-approved
in Full only; `PROVIDER_FEATURES`; runner feature). Bundle their
`skills/<x>-tools` directories into the skill catalog (embed via `include_dir!`
in `harw-registry-defaults` next to `AGENTS_DIR`, exposed as a trusted bundled
layer to `SkillIndex`) and give each a `triggers.toml` (`tools = [...]`,
`commands = ["cargo test", ...]` for cargo-tools, `error_codes` for cargo-tools
(`E0…` family via `keywords` since error codes are per-skill lists)). Profiles:
`sys.*`, `fsread.*`, `git.*` in `Full` and `ReadOnlyExplore`; `cargo.*` in `Full`.
Keep `shell.exec` untouched.

*As built in B4 (deviation, see §7):* `sys.*` is registered in `Full` only and is
**not** auto-approved. `harw-tool-sys` declares `ExecuteProcess` for all twelve
tools (it reads host state outside the workspace; `sys.env` exposes the harness
environment), no read-only profile grants `ExecuteProcess`, so the tools ask like
`process.list`. The skills are embedded by `harw-home/src/bundle.rs` with
`include_str!` from the tool crates instead of an `include_dir!` layer in
`harw-registry-defaults`. `cargo.*` beyond `cargo.test_one` is not registered yet,
so `cargo-tools/triggers.toml` carries only `tools` (no `commands`, no `E0…`
codes) until it is.

### 3.3 TUI Mission Control (PL-94 W02 + landscape anchors, PL-68 W20, PL-94 W01)

#### 3.3.1 Target screens (authoritative; cell-exact tests derive from these)

Portrait (reference 46×78; also 48×65, 70×90):
```text
┌ ACTIVE AGENTS ──────────┬ ACTIVE JOBS ───────────┐   CARDS_ROWS = 4 (title + 3)
│ ● root · thinking 29%   │ ▸ build · 82% · 18m12s │
│ ● worker · fs.read      │   tests · running 2m18s│
├ FINISHED / FAILED · ⛨ DoD 0 ───────────────────────┤   1 row (collapsed) / 3–8 rows (expanded)
│ Explorer ×8 ✓6 ✗2 NEW · Reviewer ×3 ✓3 · job ×5 ✗1  │
├ LIVE · root-orchestrator ──────────────────────────┤   header + 0..4 body lines (chat_first)
│ ∴ Checking Lens and internal tool contracts…        │
│ ● fs.read · in progress · 1.2s                      │
├ CHAT ──────────────────────────────────────────────┤   largest region, full width
│ …                                                   │
├─────────────────────────────────────────────────────┤
│ status · goal · ⛨ · approvals                        │
│ › composer                                          │
└─────────────────────────────────────────────────────┘
```
Landscape three columns (160×45, 200×50; dense profile from 120×42):
```text
┌ CHAT ───────────────────────────┬ FINISHED / FAILED ─────────┬ ACTIVE AGENTS ──────┐
│                                 │ Explorer ×8 ✓6 ✗2 NEW      │ ● root · thinking   │
│                                 │ Reviewer ×3 ✓3             │ ● worker · fs.read  │
│                                 ├ LIVE · root-orchestrator ──┼ ACTIVE JOBS ────────┤
│                                 │ ∴ reasoning tail …         │ ▸ build · 82%       │
│                                 │ ● fs.read · 1.2s           │   tests · running   │
│                                 │ (scrollable when focused)  │                     │
├─────────────────────────────────┴────────────────────────────┴─────────────────────┤
│ status / goal / approvals / composer                                              │
└───────────────────────────────────────────────────────────────────────────────────┘
```
Shallow landscape (cols > rows but not enough width for three columns, e.g.
100×30, 80×40 is portrait-stack by rule): today's `WideSide` right column
(agents above jobs stacked, 44 cols) plus a 1-row Finished strip and a LIVE
header-only strip above the chat (via `chat_first` on the chat column).
Compact/summary (39×60, 48×19): today's one-line summary; the three identities
stay reachable as `Agents 2 · Jobs 1 · Done 11/✗3` counters in that line.

#### 3.3.2 Geometry (`harw-tui-layout`, pure, layer F)
New `harw-tui-layout/src/mission.rs` (re-exported from `lib.rs`):
```rust
pub enum Orientation { Portrait, Landscape, Balanced }        // rows>cols, cols>rows, equal
pub fn orientation(cols: u16, rows: u16) -> Orientation;
pub enum MissionPlacement { PortraitStack, LandscapeThreeColumns, ShallowLandscape, CompactSummary }
pub struct MissionInput { pub cols: u16, pub rows: u16, pub status_rows: u16, pub composer_rows: u16,
    pub live_body_lines_wanted: u16, pub finished_rows_wanted: u16 /*1 collapsed, 3..8 expanded*/,
    pub agents_visible: bool, pub workbench_visible: bool, pub explorer_visible: bool }
pub struct MissionLayout { pub placement: MissionPlacement, pub orientation: Orientation,
    pub chat: Rect, pub agents: Option<Rect>, pub jobs: Option<Rect>, pub finished: Option<Rect>,
    pub live: Option<Rect>, pub live_mode: LiveMode, pub explorer: Option<Rect>,
    pub workbench: Option<Rect>, pub status: Rect, pub composer: Rect }
pub fn classify_mission(input: MissionInput) -> MissionLayout;
pub const LANDSCAPE_CHAT_MIN_COLS: u16 = 64;  pub const LANDSCAPE_CENTER_MIN_COLS: u16 = 38;
pub const LANDSCAPE_RIGHT_MIN_COLS: u16 = 30;  // dense: 48 / 34 / 26
pub const LANDSCAPE_MIN_ROWS_BODY: u16 = 5 + 5 + 3 + 7; // agents, jobs, finished, live
```
Decision order (deterministic, saturating, never overlapping; property tests):
1. `bottom = status_rows + composer_rows`; `body = rows - bottom` (saturating).
2. If `orientation != Portrait` and `cols >= 64+38+30` (or dense `48+34+26` with
   `cols >= 108`) and `body >= 20` → `LandscapeThreeColumns`: columns
   `[Min(chat_min), Length(center), Length(right)]` where `right = clamp(cols/5, 26..44)`,
   `center = clamp(cols/4, 34..60)`; explorer (36 cols) only if chat keeps ≥ chat_min.
   Centre: `finished = clamp(finished_rows_wanted, 3..8)` rows (+2 border), rest
   `live`; right: `agents = ceil(body/2)`, `jobs = rest` (each ≥ 5).
   Workbench: if visible, takes the lower 40 % of the **right** column below jobs
   (jobs/agents shrink but keep ≥ 5 rows) else hidden with a status hint.
3. Else if `placement::classify(...)` says `WideSide` → `ShallowLandscape`:
   existing `split` geometry; in the chat column apply `chat_first` with
   `cards = None` semantics (finished strip 1 row + live header only); right
   column = agents above jobs (new `split_right_column_stacked`).
4. Else if `PortraitDock` → `PortraitStack`: `chat_first` over the body (cards row
   via `split_dock`, finished strip = `finished_rows_wanted` clamped so chat keeps
   `MINI_LIVE_MIN_CHAT_ROWS`, live per `LiveMode`). Below 68 cols cards are
   `Combined` (existing).
5. Else → `CompactSummary` (today's Summary/Compact; all window rects `None`).
Invariants (tests): rect union ⊆ area, pairwise disjoint, every `Some` rect has
width≥3 && height≥3 except 1-row strips; 0×0 and 1×1 never panic; flipping
orientation on resize re-classifies with no other state.

#### 3.3.3 Window model and rendering (`harw-tui/src/mission/`)
```
harw-tui/src/mission/mod.rs         MissionWindow, WindowUi, MissionUi, focus cycle order
harw-tui/src/mission/projection.rs  pure projections (W20-A): ActiveAgentRow, RunningJobRow,
                                    TerminalRow (category, total, ok, failed, cancelled, unseen_failures, last_at, badge),
                                    fn active_agents(&AgentMonitor)->Vec<..>, fn running_jobs(&[JobRow])->Vec<..>,
                                    fn terminal_aggregate(&AgentMonitor, &[JobRow], &ProvenanceIndex, as_of)->Vec<TerminalRow>
harw-tui/src/mission/agents_window.rs   render(rows, area, buf, theme, &WindowUi, focused) + hit/scroll helpers
harw-tui/src/mission/jobs_window.rs
harw-tui/src/mission/finished_window.rs (collapsed 1-row and expanded modes; Enter opens constituent list overlay)
harw-tui/src/mission/live_window.rs     (W02 content; header `LIVE · <role>`; body = reasoning tail, tool/status line, then recency slots)
harw-tui/src/mission/stop_job.rs        (W20-E: confirmation dialog text + dispatch of `/jobs stop <id>` through CommandRegistry)
```
```rust
pub(crate) enum MissionWindow { Agents, Jobs, Finished, Live }
pub(crate) struct WindowUi { scroll: usize, selected: usize, follow: bool, viewport_rows: Cell<usize> }
pub(crate) struct MissionUi { agents: WindowUi, jobs: WindowUi, finished: WindowUi, live: WindowUi,
    finished_expanded: bool, live_expanded: bool, pinned_agent: Option<String>, seen_failures_rev: u64 }
```
- `PaneFocus` gains `Jobs`, `Finished`, `Live`. F4 cycle: Chat → Agents → Jobs →
  Finished → Live (only when `live_expanded`) → Explorer → Workbench → Chat;
  Esc → Chat. Existing `PaneFocus::Agents` keeps all current keys; the shared
  `AgentMonitor.selected`/`panel_scroll` is used **only by the Agents window and
  the legacy combined dock**; Jobs/Finished/Live use `MissionUi`.
- New `KeyAction`s (append to the enum **and** `ALL`, bump the array length, add
  defaults + help text + keybinding tests): `FocusJobs` (Alt+J), `FocusFinished`
  (Alt+F), `ToggleLiveDetail` (Alt+L), `PinLiveAgent` (`p` inside Agents window,
  non-rebindable panel key like `f`/`c`). `Ctrl+J` stays newline.
- Jobs window keys (focused): ↑/↓/j/k, PgUp/PgDn, Home/End, Enter = job detail
  (existing `render_job_detail` path), `k`… **conflict**: `k` is "up" in the
  agent list; in the Jobs window use `x` for stop (document it; W20 suggested `k`,
  but the existing list convention wins) → confirmation `ChoiceDialog`
  `Stop job "<name>" (<id>)? TERM, then the existing job stop policy. [Enter] stop [Esc] cancel`
  → on confirm dispatch the literal line `/jobs stop <JobId>` via
  `command_exec::execute_command_as` (same admission, busy class, OpContext as a
  typed command). Never call `JobManager::stop` from the key handler. After the
  refresh show the neutral state ("job is now <state>").
- Finished window: rows = `TerminalRow`s (category = agent role for agent runs,
  `tool:<origin_tool>` for tool-origin jobs, `Unknown job` without provenance);
  unseen failures show `✗ n NEW` until acknowledged (Enter/`a` on the row bumps
  `seen_failures_rev`; nothing deleted). Enter opens a constituent overlay (reuse
  `OverlayView` with a simple list; newest first). Retention: counters survive
  constituent eviction ("12 total, 8 listed"); `TERMINAL_RETENTION_PER_CATEGORY = 50`
  with a test.
- Provenance (W20-A): `harw-tool-job/src/model.rs` `JobMeta` gains
  `#[serde(default)] origin: Option<ManagedJobOrigin>` with
  `enum ManagedJobOrigin { AgentChild { child_session_id: String }, Tool { tool: String, call_id: Option<String> }, Operator }`;
  reader maps legacy `origin_tool`/`origin_call_id` → `Tool`, both absent →
  `Unknown` (never guessed from `name`); `JobEvent::Started` carries it;
  `harw-agent-runner/src/job_child_backend.rs` sets `AgentChild` on
  `start_piped…` (add an `origin` parameter or a `JobStartSpec` field — pick the
  smaller diff). `JobRow` gains `origin: ManagedJobOrigin`. Correlation rule: an
  agent run with a `child_session_id` owns the terminal count; a job tagged
  `AgentChild` whose run is unknown is shown as "Agent child (pending)" and never
  counted as its own category (no promotion to "untracked" without a watermark —
  W20 §4.2).
- Live window (W02): source = selected agent (`pinned_agent` or the active root
  orchestrator, else the highest-ranked `LiveActivityRanker` candidate); content
  from `AgentLive` only (`reasoning_preview` tail → `∴ …`, current tool →
  `● name · state · elapsed`, last status), then recency slot lines; all through
  `sanitize_inline`; "no visible events yet" when empty. Portrait: header + ≤4
  body lines, **non-focusable, non-scrolling**; landscape centre: scrollable when
  focused (`ChatScroll`), `Alt+L` toggles an expanded detail (existing
  `render_agent_detail`) inside the same rect. No model call, no new job, no
  timer beyond the existing frame requests.
- `FrameRegions` gains `jobs`, `finished`, `live`; mouse wheel/click route by
  `Rect::contains(Position)`; modal dialogs keep precedence.
- Portrait branch in `render_viewport`: replace `render_dock(split_dock_areas(dock))`
  + `portrait_live_strip` with `classify_mission` → per-window renderers. Keep
  the legacy `Combined` dock renderer for < 68 cols. Keep every existing portrait
  test green (R19 regression contract: `narrow_tall_pane_shows_the_agent_dock`,
  `narrow_status_line_moves_marks_to_their_own_row`, `host_warning_stays_complete…`,
  `portrait_live_strip_follows_chat_first_at_46x78`, `ctrl_l_requests_a_full_redraw`).
- Refactor boundary: move the body composition out of `app.rs` into
  `harw-tui/src/app/render.rs` (`fn compose_body(..) -> BodyAreas`) so
  `render_viewport` becomes overlay → body → status → composer. **Do not** split
  `app.rs` further in this plan; the goal is testable composition, not a rewrite.

#### 3.3.4 Inline activity-ranked live cells (W01, M2a + M2c)
- `harw-tui/src/app/live_slots.rs`: `LiveSlotProjection` computed per frame from
  `LiveActivityRanker` output + `ChildStreamRegistry`: a **virtual** cell list =
  canonical cells (unchanged, append-only) with up to `MAX_INLINE_LIVE = 2`
  virtual `LiveChildCell`s inserted at slot #2/#3 from the tail per the W01 slot
  table (idle: #2; user message submitted / UIA in flight: #3; after final reply:
  #2); the remaining running runs collapse into one `+N weitere aktiv` row.
  Virtual cells render the existing `ChildStreamBlock` (bounded) with a `▲ pinned`
  marker when pinned.
- Anchoring (M2c): `ChatScroll` gains an identity anchor `Option<CellId>` set when
  the user scrolls away from the tail; while anchored, slot reordering is frozen
  and a `N neue Aktivitäten` row appears at the bottom; returning to tail resumes.
- Never touches export (`export_capture.rs`), session persistence, model context
  or `final_reply.rs` dedupe (tests assert the canonical cell vector is identical
  before/after).

### 3.4 Execution chain: Bubblewrap ∘ Landlock ∘ cgroup v2 ∘ DoD ("connect everything")

Goal: every model-driven process (managed job, child agent, `shell.exec`) runs
**inside one cgroup per attempt**, **inside a Bubblewrap namespace**, with
**Landlock + NO_NEW_PRIVS + capability drop + rlimits applied inside that
namespace by the trampoline**, and **DoD observes and (when enabled) enforces
on exactly those cgroups**. No new mechanism; composition of the existing ones.

```text
harw (delegated cgroup root R, e.g. …/app.slice/harw.slice)
 └─ R/harw-job-<attempt>              ← created by LinuxExecutor (limits: JobResources)
     └─ bwrap --unshare-all --unshare-net … \
          --ro-bind <harw-job-exec> /run/harw-job/exec \
          --bind <attempt-dir> /run/harw-job/attempt \      (plan 0600 + report)
          -- /run/harw-job/exec --plan /run/harw-job/attempt/plan.json --report …/report.json
            └─ trampoline: verify /proc/self/cgroup == R/harw-job-<attempt>
                           → setrlimit → NO_NEW_PRIVS → cap drop → Landlock(ruleset over the
                             namespace's view: workspace dest rw, /usr … ro+exec, /tmp rw)
                           → exec <program>
DoD: profile "harw-jobs" scope=cgroups, cgroup_paths=[R], include_descendants=true
     sentinel --cgroup-root R  → harw-dod-cgroup rows = harw-job-<attempt>
     probe-bpf exec/tcp events attributed to R/harw-job-<attempt>
     [dod] allowed_cgroup_prefixes = [R]  → warden may freeze/kill only harw jobs
```

**E1 — `LinuxSandboxBackend::BwrapTrampoline`** (new variant; `harw-job-runtime/src/coordinator/linux.rs`,
`harw-job-executor-bwrap/src/executor.rs`, `harw-job-exec/src/plan.rs`):
- `BwrapExecutor::plan_with_trampoline(spec, policy, TrampolineMount { exec_path,
  attempt_dir })` → the bwrap argv binds the trampoline binary ro at
  `/run/harw-job/exec` and the attempt dir rw at `/run/harw-job/attempt`, and
  runs `harw-job-exec --plan … --report …` as the sandboxed command (relay
  prefix preserved for proxy-only egress).
- `ExecPlanV1` gains `cgroup: CgroupJoin::{Attach{..} (existing), Verify {
  expected_proc_path }}`; `Verify` reads `/proc/self/cgroup` and refuses to
  exec (`exit_code::SANDBOX_REFUSED`) if the path differs — this closes the
  spawn-to-attach gap semantically (outside attach via `spawn_in_cgroup` stays;
  the job never runs an instruction outside its cgroup).
- Landlock rules are planned against the **in-namespace** view: `plan_landlock`
  gets a `PathMap` (host path → sandbox dest) derived from the bwrap plan
  (workspace dest, always-bound trees, `/tmp`, `/run/harw-job/attempt` rw for the
  report). Read-only binds get read-only Landlock access **without** execute
  except the system exec trees — the distinction bwrap cannot make.
- `SandboxReport` = bwrap predicted report **merged** with the trampoline's
  actual report (namespace dims from bwrap, filesystem/landlock/caps/nnp/rlimits
  from the report file); `resource_limits` becomes `Enforced` when both the
  cgroup limits and rlimits applied.
- Backend selection order (`harw-command/src/port.rs`, `verify_sandbox.rs`, one
  shared resolver `harw_job_runtime::select_backend(HostReport, config)`):
  BwrapTrampoline if bwrap found + trampoline binary found; else Bwrap; else
  LandlockTrampoline; else None — each step reported, never silent.
  **`JobCommandPort::host` must call it** (today it hard-codes `None`/no
  cgroup root — §2.3b); `harw-command`/`harw-runtime` must enable the `bwrap`
  feature of `harw-job` (today only `harw-cli` does). Root `Makefile`
  `build`/`install` (and `harw-install`'s asset list) gain `harw-job-exec`,
  installed next to `harw`; `harw doctor` reports it missing otherwise.
- Tests: pure plan tests (argv contains both binds and the exec path; relay
  prefix order), `CgroupJoin::Verify` unit tests with fake `/proc/self/cgroup`
  content, and an `#[ignore]`d integration test (host probe gated) that runs
  `harw verify-sandbox --backend bwrap-trampoline` and asserts the merged report
  dims; extend `harw-cli/src/verify_sandbox.rs` with `--backend` and a
  per-dimension table output.

**E2 — `shell.exec`, `job.start` and child agents through the sandboxed
runtime path** (`harw-tool-shell/src/exec.rs`, `harw-tool-job/src/manager.rs`
`ShellJobLauncher`, `harw-agent-runner/src/job_child_backend.rs`, `harw-command`):
stop pre-building a `prlimit … -- bwrap …` argv and submitting it as
`CommandSandbox::Host`. Instead submit a `JobSpec` with `program = /bin/sh -c …`
(or the child-agent program), cwd = workspace, a `SandboxPolicy` derived from
the tool's `SandboxSpec` (`harw-job-executor-bwrap` already maps it:
workspace rw/ro, `NetworkMode`, cargo/tmux modules via `BwrapLauncher::with_profile`
— extend `BwrapExecutor::plan` to accept the `harw_sandbox::SandboxProfile`
modules and `HostPathBinding` so nothing `shell.exec` can do today is lost),
`ResourceRequest` from `ShellLimits` (map all of `memory_max`, `memory_high`,
`cpu_max`, `pids_max`, `io` and the rlimits — fix `job_resources` in
`coordinator/linux.rs:~349`), timeout → deadline, stdio piped; keep today's
output caps/truncation/cancel semantics. Child agents: `JobChildBackend`
submits with `SandboxProfileName::WorkspaceBuild` (or the role's profile) and
`executed_on_host = false`; the host-permit path (`run_host_command`) stays the
only `Host` submission. Fallback when `select_backend` → `None`: today's
behaviour, logged once per session as `runs without cgroup/landlock`. Config
`[sandbox] execution = "chain" | "legacy"` (default `chain`). Tests: fake port
receives one `JobSpec` per call with the expected policy; output/timeout parity
against the existing `spawn_and_collect` fixtures; a child agent's job record
carries a cgroup path.

**E3b — DoD observes cgroups created after attach** (the BPF scope is static
today): (1) userspace first — `harw-probe-bpf` rescans the profile's
`include_descendants = true` roots on a bounded interval (default 2 s, inotify
on `cgroup.events`/directory create where available) and updates
`SCOPE_CGROUP_IDS` incrementally (add new ids, retire removed ones; bounded map
size with a counted overflow); (2) optional, separate commit — kernel-side
ancestor match: for roots with descendants, store `(root_cgroup_id, level)`
and compare `bpf_get_current_ancestor_cgroup_id(level)` in the exec/connect
programs (`dod/bpf/src`, own build domain; bump the wire/ABI version) so no
rescan window exists. Tests: a fixture loader (`FixtureBpfLoader`) sees a
cgroup created after attach within one rescan; map overflow counted, never
silent. Also fill `Actor.cgroup` (E4) so the observation is attributable.

**E3 — one cgroup-root contract shared by harw and DoD**:
- `harw_job_runtime::host::resolve_delegated_root(config, env) -> RootResolution
  { root: Option<PathBuf>, source: Config|Env|OwnCgroupDelegated|None, hint }`:
  order `[jobs] cgroup_root` (new config key, profile scope, trusted layers
  only) → `HARW_CGROUP_ROOT` env → own cgroup if `cgroup_v2_delegated` → None
  with hint "start harw under `systemd-run --user --scope -p Delegate=yes …`
  or enable `harw-control.service` (Delegate=yes)". Add `Delegate=yes` +
  `DelegateSubgroup=harw.slice` *(verify systemd version ≥ 254 for
  DelegateSubgroup; else document the `systemd-run` route)* to
  `deploy/systemd/harw-control.service`.
- `harw dod config suggest` (D3 CLI) prints the DoD profile stanza for the
  resolved root (`scope = "cgroups"`, `cgroup_paths = ["<R absolute>"]`,
  `include_descendants = true`, `sensors = ["exec","tcp-connect"]`) **and** the
  matching `[dod] allowed_cgroup_prefixes = ["<R>"]` for harw's config;
  `harw dod doctor` checks that the active DoD profile covers R and that
  `allowed_cgroup_prefixes` ⊆ R (warns otherwise). **Canonical prefix form:**
  one documented form (absolute path under the cgroup2 mount) with a converter
  to the Warden's relative `CgroupId`; `DodSection::validate` rejects the other
  form. Wiring the prefixes into the Warden's `ProofPolicy` stays with PL-96 W01
  (v2 proof) — out of scope; say so in `docs/setup/dod.md`.
- Sentinel unit: `--cgroup-root <R-absolute>` (existing flag) via a systemd
  drop-in that `harw dod config suggest --drop-in` also prints; document in
  `docs/setup/dod.md` "Observe harw jobs".
- Tests: resolver order with fake env/config; `suggest` output golden; doctor
  warnings.

**E4 — identity correlation (jobs ↔ findings)**: surface the job's cgroup path
(already persisted: `JobProcessIdentity.cgroup_path` in `harw-tool-job/src/model.rs:410-420`
meta v2, and `LinuxRecoveryIdentity.cgroup_path`) as `JobStatus.cgroup_path` →
`JobRow.cgroup: Option<String>`. **Attribution on the DoD side:** the probe
already receives `cgroup_id` in the wire header; `harw-probe-bpf/src/collect.rs:~329`
forwards it, and the **sentinel** (unprivileged) resolves id → path by reading
the inode numbers of the cgroup directories beneath its `--cgroup-root`
(`harw-dod-readfs` metadata; cached, refreshed on miss, bounded) and sets
`Actor.cgroup = Some(path)`. Extend the sentinel export line (D1,
`export.rs::encode_line`) with `schema_version: 2` and optional
`scope: Option<String>` = that path — additive, old readers (`security_signals`,
security-hub) ignore it. `harw-dod-client::correlate(&[FindingLine],
&[JobRow])` matches `scope` by **path-component prefix** of a job's cgroup path;
TUI Jobs window shows `⛨ n` on that row; `dod.findings` accepts `job_id` and
`harw dod findings --job <id>`.

**E5 — honest status everywhere**: `harw doctor` (existing) + `harw dod doctor`
+ `harw verify-sandbox` + the TUI status mark use one `ExecutionChainReport
{ bwrap, trampoline, landlock_abi, cgroup_root, dod_profile_covers_root,
warden }` built from `HostReport` + `RootResolution` + `DodStatus`.

Non-goals here: seccomp (PL-20 §4.3), user-namespace tricks beyond what bwrap
does, Kata/OCI/K8s executors, changing the Warden TCB.

### 3.5 Semantic activity and macros (PL-71 + `permission_request!`)
- New crate **`harw-activity`** (layer `I`; deps `serde`, `serde_json` only):
  ```rust
  pub struct ActivityDescriptor { pub domain: &'static str, pub verb: &'static str, pub resource_field: Option<&'static str>, pub effect: Effect }
  pub enum Effect { Observe, Mutate, Execute, Communicate, Create, Delete, Control, Other }
  pub enum ActivityKey { Path(String), Job(String), Call(String), Agent(String), Other(String) }
  pub struct ActivityEvent { pub ingest_seq: u64, pub owner: ActivityOwner, pub descriptor: ActivityDescriptor, pub resource: Option<ActivityKey>, pub lifecycle: Lifecycle, pub outcome: Option<Outcome> }
  pub struct PatternId(&'static str); // "file-read-burst@1", "file-edit-read-loop@1", "job-lifecycle@1", "agent-role-failure-burst@1"
  pub struct PatternInstance { pub id: ProjectionId, pub pattern: PatternId, pub key: Option<ActivityKey>, pub state: PatternState, pub constituents: Vec<u64>, pub revision: u64 }
  pub struct Reducer { .. } impl Reducer { pub fn apply(&mut self, ev: &ActivityEvent) -> Vec<ProjectionUpdate> }
  ```
  Deterministic (order = `ingest_seq`; time only from events; rebase
  `Call(C)→Job(W)` keeps the `ProjectionId`); failure counts always visible;
  unknown descriptor ⇒ standalone instance (I5). Property tests + replay tests
  from recorded fixtures.
- `harw-macros` `#[tool]`: new optional key
  `activity(domain = "filesystem", verb = "read", resource = "path", effect = "observe")`
  and optional `skill = "<bundled skill name>"`. Generates on the wrapper struct
  `pub const ACTIVITY: Option<harw_activity::ActivityDescriptor>` and
  `pub const SKILL: Option<&'static str>`; `resource` must name an existing arg
  field (compile error otherwise — trybuild test); unknown `effect`/duplicate
  `activity` ⇒ compile error. `tool_provider!` exposes `TOOL_ACTIVITIES` and
  `TOOL_SKILLS`. **Nothing enters `ToolSpec`.** Hand-written tools get a
  `harw-registry-defaults/src/activity_map.rs` table (fs.*, doc.read_pdf, job.*,
  process.*, shell.exec) consulted via `activity_for(tool_name)`.
- `permission_request!` (`harw-macros`, authority doc §6.3): validated
  declarative permission list → `harw_authority::PermissionRequest` only; unknown
  entry ⇒ compile error; no grant construction; trybuild tests. Use it in one
  real call site (the new `dod.*` ops' required permissions) so it is not dead.
- TUI adapter: `harw-tui/src/app/activity_glue.rs` feeds `ToolCell` lifecycle
  events into a `Reducer`; `ToolGroupCell` grouping is driven by
  `file-read-burst@1` / `file-edit-read-loop@1` (descriptor `effect == Observe`
  and same `Path` key) instead of the hard-coded name list — keep the current
  visual output byte-identical for the existing cases (tests exist in
  `history_cell.rs`); `job-lifecycle@1` collapses `job.start/status/logs/wait`
  on one `Job(W)` into one updating cell; `agent-role-failure-burst@1` backs the
  Finished window's unseen-failure badge. Expansion (Ctrl+O) reveals constituents (I6).

---

## 4. Work packages (ordered; each lists files, deliverable, tests, gate)

Legend: **D** DoD suite · **B** triggers/tool landscape · **C** TUI · **M** macros/activity · **E** execution chain.
Dependencies: C1→C2→C3→C4(→C4b); C5 after C3; D1→D2→D3→D4→D5; B1→B2→B3; M1→M2→M3 (M3 after C3);
E1→E2; E3 needs D3 (CLI) and D2 (client); E3b independent (DoD only); E4 needs
E3 + E3b + C4 + D2; E5 needs E3 + D3.
D4 needs B1 (trigger wiring) only for the end-to-end test; D5 needs C3.
Recommended execution order:
**C1, C2, C3, C4, D1, D2, D3, E1, E2, E3, E3b, B1, B2, B4, D4, E4, B3, M1, M2, M3, D5, E5, C4b, C5, B5, X1 (+X1b)**.

### C1 — Mission geometry (harw-tui-layout)  [PL-94 M1 / W02 U0 completion]
- Files: `harw-tui-layout/src/mission.rs` (new), `lib.rs` re-exports, `placement.rs`
  (expose `WIDE_AGENT_PANEL_MIN_COLS` reuse), README of the crate.
- Deliver §3.3.2 exactly. Tests: the matrix 46×78, 48×65, 70×90, 60×60, 80×40,
  100×30, 120×42, 160×45, 200×50, 39×60, 48×19, 1×1, 0×0 + a proptest-style loop
  over 0..=250 × 0..=120 asserting the invariants; orientation flip test.
- Gate: `cargo test -p harw-tui-layout`, arch gate (no new deps).

### C2 — Three status windows + LIVE window, portrait  [W02 U1/U2]
- Files: `harw-tui/src/mission/{mod,projection,agents_window,jobs_window,finished_window,live_window}.rs`,
  `panes.rs` (`PaneFocus` variants, `MissionAreas` conversion from `MissionLayout`),
  `app/render.rs` (new `compose_body`), `app.rs` (portrait branch swap, `MissionUi`
  field on `ChatApp`), `agent_monitor.rs` (expose read accessors needed by
  projections; no second store), `jobs_panel.rs`, `keybindings.rs`, `help_overlay.rs`.
- Deliver: portrait layout of §3.3.1 with independent scroll/selection per
  window; collapsed Finished strip; LIVE from selected agent (W02 source rules);
  focus cycle; new key actions; `FrameRegions` extension; help text.
- Tests (TestBackend): `render_viewport_at_46x78_shows_three_windows_live_and_chat`,
  `…48x65…`, `…70x90…`, the existing R19 tests unchanged, focus-cycle test,
  wheel-over-each-window routes only that window, LIVE never steals composer keys,
  multi-paragraph agent prose renders ≤ 4 sanitized lines (W02 acceptance 1–3, 9).
- Gate: `cargo test -p harw-tui` + full gates.

### C3 — Landscape three columns + shallow landscape  [PL-94 anchors]
- Files: `app/render.rs`, `panes.rs` (`split_right_column_stacked`), window renderers (scrollable LIVE when focused, `Alt+L` detail).
- Tests: 160×45 and 200×50 five bordered windows at exact anchors (Finished
  top-centre, LIVE below, Agents above Jobs far right, Chat left and widest);
  120×42 dense or shallow deterministic; 100×30 shallow with strips; Explorer/
  Workbench/F11 preserved (200×50 + Workbench test); resize flip keeps selection
  and running jobs (W02 acceptance 2, 4, 7).

### C4 — Terminal aggregates, provenance, job stop  [W20-A/C/D/E]
- Files: `harw-tool-job/src/{model,event,manager}.rs`, `harw-agent-runner/src/job_child_backend.rs`,
  `harw-tui/src/jobs_panel.rs`, `app/jobs_glue.rs`, `mission/{projection,finished_window,stop_job}.rs`,
  `choice_dialog.rs` reuse, `command_exec.rs` (typed helper `dispatch_line`).
- Tests: legacy `meta.json` fixture → `Unknown`/`Tool`; agent+job same child
  counted once; pending-link row; diverging outcome badge; `x` → dialog → exactly
  one `/jobs stop <id>` reaching the `jobs` adapter (mock registry); rejected
  admission produces no second path; NEW badge ack; retention constant test.

### D1 — Harw-DoD naming + stale-doc fixes + export pipeline
- Files: `README.md` (DoD section), `docs/setup/dod.md`, `dod/README.md`,
  `docs/design/harw-dod-charter.md` (§2 note: "Harw-DoD" is the suite name,
  `harw-dod` the crate), `dod/crates/harw-dod/{src/lib.rs,Cargo.toml}` (cgroup
  not a scaffold; 34 crates), `deploy/systemd/harw-sentinel.service` (`--config`,
  `--findings-export`, `--health-export`), `deploy/tmpfiles.d/harw.conf`,
  `deploy/sysusers.d/harw.conf` (`harw-dod-readers`), `harw-install` fixtures/
  parity tests, `dod/packaging/manifest`, `dod/crates/harw-sentinel/src/{cli,main,export}.rs`
  (`--health-export`), `harw-config/src/dod_toml.rs` (`export_dir`,
  `findings_window_hours`, doc fix, scope table in `scope.rs` + `merge.rs`),
  `harw-runtime/src/security_signals.rs` (path resolver),
  `dod/crates/harw-sentinel/src/export.rs` (export line `schema_version: 2` +
  optional `scope`, additive; see E4).
- Tests: sentinel health export round-trip (tempdir), atomic write; export v1
  lines still parse by every reader; install parity; `dod/scripts/stage-test.sh`
  still passes; config scope exhaustive test updated
  (`harw-config/tests/config_scope_exhaustive.rs`).

### D2 — `harw-dod-client` crate
- Files: `harw-dod-client/{Cargo.toml,src/lib.rs,src/{paths,findings,health,config,units,status}.rs}`,
  root `Cargo.toml` members, `xtask/arch-policy.toml` (layer I; allowed deps),
  `deny.toml` unchanged.
- Tests: tail reading bounded (256 KiB, 16 KiB lines, symlink refused), summary
  windowing with injected `now`, config valid/missing/invalid/untrusted, `systemctl`
  absent ⇒ `NotAvailable` (fake via `PATH` in test), status assembly.

### D3 — `harw dod` CLI + `dod` operations + model tools + agents
- Files: `harw-cli/src/cli/{mod,dod}.rs`, `harw-cli/src/dod_cmd.rs`, `docs/cli.md`,
  completions test; `harw-ops/src/dod.rs`, `lib.rs::register_all`, `ServiceMap`
  wiring in `harw-runtime/src/assembly.rs` (`DodService`); `harw-registry-defaults/src/{capability_catalog,authority,lib,profile}.rs`;
  `agents/roles/security-*-triage/*.toml` + golden IR; `harw-ops/tests/approval_declaration_gate.rs` fixture.
- Tests: clap tree (every subcommand has help, `--json` where applicable, exit
  code 3 when not root for enable/warden), op output snapshots with a fake
  `DodService`, model tool schema (`OpArgs`) round-trip, catalog parity test,
  readonly ops auto-approved, triage roles admit exactly the three tools.

### D4 — Bundled `harw-dod` skill with `triggers.toml`  (after B1)
- Files: `harw-home/assets/skills/harw-dod/{skill.toml,instructions.md,triggers.toml}`,
  `security.toml` family binding.
- Tests: `SkillIndex` loads it with zero warnings; `select()` fires on
  `TriggerEvent::Tool("dod.findings")` and on keyword "egress"; end-to-end:
  a fake-provider turn that calls `dod.status` gets the `skills.triggered`
  fragment once, not twice (dedupe), within budget.

### D5 — TUI DoD surfaces  (after C3)
- Files: `harw-tui/src/dod_view.rs` (`OverlayView`), `status_line.rs` (`⛨` mark +
  `fit_segments` priority test), `mission/finished_window.rs` (title suffix),
  `app.rs` fetch wiring, `help_overlay.rs`.
- Tests: `/dod view` opens the overlay and renders units/sensors/findings from a
  fixture; mark hidden when not installed; mark survives narrow widths after the
  host warning (reuse `host_warning_stays_complete…` pattern).

### B1 — Trigger injection wired into the turn (PL-95 S3)
- Files: `harw-core/src/skill_injection.rs` (new), `turn_loop.rs` (`assemble_round_request`
  feed + fragment placement + compaction reset hook), `harw-context/src/sources.rs`,
  `harw-config/src/skills_toml.rs` (new `[skills]`, scope table, merge, defaults
  round-trip test), `harw-runtime/src/assembly.rs` (build `TriggerIndex` once per
  assembly from `SkillIndex`; replace `skill_catalog_hint` with `catalog_lines`),
  session-store `injection` event type, `docs/design/config-structure.md` §skills.
- Tests: deterministic injection from recorded history (keyword, error code in
  tool result, first tool use, path, command); per-turn and per-session budget
  with `Dropped` reasons; dedupe across turns; reset after compaction; disabled
  by config ⇒ no fragment; imported skill ⇒ `Evidence` trust; cache-stable prefix
  untouched (assert the stable instruction fragments are byte-identical across
  turns).

### B2 — Tool→skill link, `tool.index`, prompt block, lint (S4 + PL-88)
- Files: `harw-registry-defaults/src/tool_index.rs` (`skill` field, reverse map),
  `harw-ops/src/tool_index_op.rs`, `harw-instructions/src/baseline.rs` (`## Tool
  families` block), `harw-cli/src/cli/tool.rs` (`harw tool list|describe`),
  `profile.rs` (pass the index into `AgentIdentity`), registry lint test.
- Tests: `tool.index` output for `family = "dod"`; prompt block stable across
  turns; first use of `sys.ps` triggers `sys-tools` even without a `triggers.toml`;
  lint fails on a fixture tool with a 10-char description.

### B3 — `harw skill …` and `/skills preview|validate` (S2)
- Files: `harw-cli/src/cli/skill.rs`, `harw-ops/src/skills.rs`, `docs/cli.md`.
- Tests: `preview` prints the same `Selection` as B1 would inject for the given
  inputs; `validate` rejects a `triggers.toml` with 17 keywords with the exact
  warning text from `skill_triggers.rs`.

### B4 — Register PL-93 internal tool crates + bundle their skills + triggers
- Files: `harw-registry-defaults/{Cargo.toml,src/{profile,authority,lib,capability_catalog,embedded_skills(new)}.rs}`,
  `harw-tool-{sys,cargo,fsread,gitread}/skills/*/triggers.toml` (new),
  `harw-agent-runner/Cargo.toml`, `harw-catalog` (new bundled layer source),
  `docs/planning/93-common-command-tools/README.md` status → PARTIALLY LANDED.
- Tests: catalog parity, `tool_permission` parity with each crate's
  `TOOL_PERMISSIONS`, profiles contain the expected names, skills discoverable via
  `skills.search`, `cargo test` command trigger fires `cargo-tools`.
- As landed: `fsread.*`/`git.*` in `Full` + `ReadOnlyExplore` (auto-approved),
  `sys.*` in `Full` only (`ExecuteProcess`, asks like `process.list`), runner
  features `tool-fsread`/`tool-gitread`/`tool-sys` reported by `--capabilities`,
  the four skills bundled through `harw-home` with a `triggers.toml` each.
- **Deferred (open):** registering the other eight `cargo.*` tools in `Full`
  (`CargoToolProvider` over the `shell.exec` executor, `ExecuteProcess`,
  auto-approved in `Full` only, which needs a per-profile approval rule that does
  not exist today), and with it the `commands` and `E0…` error-code triggers of
  `cargo-tools`. Until then `cargo-tools` fires only on first use of a `cargo.*`
  tool, and the `cargo test` command-trigger test is inverted (it must **not**
  fire).
- **Depends on B1:** the command triggers of `fsread-tools`, `git-read-tools` and
  `sys-tools` (`ls`, `git status`, `ps`, …) also fire in sessions that have
  `shell.exec` but not those tools (e.g. `executor`/`ShellExecution`, the root in
  Explore/Plan mode). The injector must skip a triggered skill whose declared
  `tools` are all absent from the session's registered tool names (owner: B1).

### B5 — CLI rules test (S6, minimal)
- A test walking the clap tree: every non-hidden command has help text and either
  uses `Printer` JSON or calls `require_text`; lists permanent aliases explicitly.

### M1 — `harw-activity` crate
- Files: `harw-activity/{Cargo.toml,src/{lib,descriptor,event,pattern,reducer}.rs}`,
  members + arch-policy (layer I), fixtures under `harw-activity/tests/fixtures/*.jsonl`.
- Tests: the four v1 patterns; concurrency (`Read(A)#1/#2` overlap); rebase keeps
  `ProjectionId`; failure visibility; replay equality; I5 unknown ⇒ standalone.

### M2 — Macros: `activity(...)`, `skill = ..`, `permission_request!`
- Files: `harw-macros/src/{tool,lib,permission_request(new)}.rs`,
  `harw-tools/src/provider_macro.rs` (`TOOL_ACTIVITIES`, `TOOL_SKILLS`),
  `harw-macros/tests/{tool_activity.rs,compile_fail/*.rs,permission_request.rs}`,
  `harw-registry-defaults/src/activity_map.rs`, annotate `harw-tool-fs` read/edit
  tools (first migration wave only, PL-71 §Migration) and the `dod.*` ops with
  `permission_request!`.
- Tests: generated constants; `resource` not a field ⇒ compile error; unknown
  effect ⇒ compile error; `ToolSpec` JSON unchanged for annotated tools (golden).

### M3 — TUI activity adapter (after C3)
- Files: `harw-tui/src/app/activity_glue.rs`, `history_cell.rs` (`ToolGroupCell`
  driven by reducer output; constituents on Ctrl+O), `mission/finished_window.rs`
  (failure-burst badge source).
- Tests: existing `ToolGroupCell` tests byte-identical; `fs.edit(A)` then 3×
  `fs.read(A)` renders `A · Edit ×1 · Read ×3`; job lifecycle collapses into one
  updating cell; mutating + failed constituent keeps the ✗ visible.

### C4b — 2×Ctrl+C hard stop (salvage from `ccr-163653c3-ezvkn6`)
- Files: `harw-tui/src/{hard_kill,process_tree}.rs` (cherry-picked hunks),
  `input_reader.rs`, `keybindings.rs`, `runtime_root.rs`, `app.rs` footer hint.
- Deliver: second Ctrl+C within the quit-hint window terminates running agent
  children and managed jobs **through `JobManager::stop` / cgroup kill (E1
  identities)**, never by walking raw PID trees when a cgroup is known; exit 130.
- Tests: the branch's tests ported; a test that a job-backed child is stopped
  through the job path.

### E1 — `BwrapTrampoline` backend (bwrap ∘ harw-job-exec ∘ cgroup)
- Files: `harw-job-executor-bwrap/src/executor.rs` (`plan_with_trampoline`,
  `TrampolineMount`, `PathMap`), `harw-job-exec/src/{plan,trampoline,report}.rs`
  (`CgroupJoin::Verify`, report merge helpers), `harw-job-linux/src/sandbox.rs`
  (`plan_landlock_mapped(policy, &PathMap)`), `harw-job-runtime/src/coordinator/linux.rs`
  (new variant + `select_backend`), `harw-job-runtime/src/host.rs`,
  `harw-cli/src/verify_sandbox.rs` (`--backend`, dimension table),
  `harw-command/src/port.rs` (use `select_backend`), docs
  `docs/design/mediated-process-execution.md` (new §"composition").
- Tests: see §3.4 E1; plus `cargo run -q -p xtask -- gates` (layer J must not
  gain a `*-sys` crate; `harw-job-exec` stays in `gate_privileges`).

### E2 — `shell.exec`, `job.start` and child agents via the sandboxed runtime path
- Files: `harw-tool-shell/src/exec.rs` (+ a `JobSpec` builder module),
  `harw-tool-job/src/manager.rs` (`ShellJobLauncher`), `harw-agent-runner/src/job_child_backend.rs`,
  `harw-job-executor-bwrap/src/executor.rs` (profile modules, host-path binding),
  `harw-job-runtime/src/coordinator/linux.rs` (`job_resources` full mapping),
  `harw-command/src/port.rs`, `harw-config/src/harness_config.rs` (`[sandbox]
  execution`, scope table, merge), `docs/design/mediated-process-execution.md`.
- Tests: §3.4 E2; existing `harw-tool-shell` tests stay green with the fake port.

### E3 — shared cgroup-root contract + DoD profile suggestion
- Files: `harw-job-runtime/src/host.rs` (`resolve_delegated_root`),
  `harw-config` (`[jobs] cgroup_root`), `deploy/systemd/harw-control.service`
  (`Delegate=`), `harw-install` parity fixtures, `harw-cli/src/dod_cmd.rs`
  (`config suggest`, doctor checks), `docs/setup/dod.md`, `docs/setup/install.md`.
- Tests: §3.4 E3.

### E3b — DoD sees new cgroups (dynamic BPF scope)
- Files: `dod/crates/harw-probe-bpf/src/{profile,collect,main}.rs`,
  `dod/crates/harw-dod-bpf/src/profile.rs` (incremental map updates), tests with
  `FixtureBpfLoader`; optional second commit in `dod/bpf/src` + `harw_dod_wire_v1.h`
  (ancestor match, ABI bump, `dod/bpf/toolchain.lock.toml` unchanged).
- Gate: `make -C dod test`, `xtask gates` (privileges unchanged: no new capability).

### E4 — jobs ↔ findings correlation
- Files: `harw-tool-job/src/model.rs` (`cgroup_path` surfaced in `JobStatus`),
  `harw-tui/src/jobs_panel.rs` (`JobRow.cgroup`), `dod/crates/harw-probe-bpf/src/collect.rs`
  (forward `cgroup_id`), `dod/crates/harw-sentinel/src/{sensors,findings,export}.rs`
  (id→path resolver, `Actor.cgroup`, export v2 `scope`),
  `harw-dod-client/src/correlate.rs`, `harw-ops/src/dod.rs` (`job_id` filter),
  `harw-tui/src/mission/jobs_window.rs` (`⛨ n` badge).
- Tests: correlation by prefix (never substring: `R/harw-job-1` must not match
  `R/harw-job-10`), badge render test with a fixture.

### E5 — `ExecutionChainReport`
- Files: `harw-job-runtime/src/host.rs` or new `harw-dod-client/src/chain.rs`
  (whichever avoids a layer inversion — check `arch-policy`), consumers:
  `harw doctor`, `harw dod doctor`, `harw verify-sandbox`, TUI status mark.
- Tests: table output golden for the four host situations (full chain / no
  bwrap / no delegation / no DoD).

### C5 — Inline activity-ranked live cells (W01 M2a + M2c)  [last; go/no-go]
- Files: `harw-tui/src/app/live_slots.rs`, `chat_scroll.rs` (identity anchor),
  `app.rs` history rendering, `child_stream_glue.rs`.
- Tests: W01 acceptance 1–10 (slot #2/#3 transitions; three concurrent runs;
  500 reasoning deltas cannot outrank a milestone; scroll-up freezes; pin; terminal
  run exits live ranking; canonical cells and export unchanged).

### X1 — Docs, ledger, status labels (final commit)
- `docs/planning/94-adaptive-tui-mission-control/README.md` + W00/W01/W02 status
  → `PARTIALLY LANDED`/`IN IMPLEMENTATION` with the implementation SHAs and test
  names; `68-mobile-tui/CONTROL-DECK-V2.md` + W20 status; `71-semantic-activity-patterns`
  status; `95-cli-tools-skills` cycles S1–S5 marked; `87-tool-gaps`; `50-dod-integration/README.md`
  new "Harw-DoD suite" section; `docs/design/tui-command-contract.md` §8 (new keys,
  `/dod`, `/skills preview`), `docs/design/tui-architecture.md` (mission module),
  `docs/cli.md` (`harw dod`, `harw tool`, `harw skill`), `docs/setup/dod.md`
  (operator flow: install → config → enable → `harw dod status`), `MIGRATION_LEDGER.md`
  `MIG-016 … MIG-019` entries, `CHANGELOG.md`.

---

## 5. Verification

### 5.1 Automated (every WP)
Gate sequence §0.3. Additionally per area:
- Layout matrix tests in `harw-tui-layout` (C1) and render-matrix tests in
  `harw-tui` (C2/C3) — the sizes listed in §3.3.2/§4 are mandatory test names.
- `cargo test -p harw-macros` includes trybuild compile-fail (M2).
- `make -C dod test` + `dod/scripts/stage-test.sh` (D1).
- `cargo run -q -p xtask -- gates` must stay green after adding `harw-dod-client`
  and `harw-activity` (layer I; no `*-sys` crates; no warden edge).

### 5.2 Manual end-to-end (record in the PR description with the exact SHA)
```bash
# TUI at the reference sizes (tmux gives exact cell dimensions)
tmux new-session -d -s harw -x 46 -y 78 'cargo run -q -p harw -- --home /tmp/harw-e2e chat'
tmux resize-window -t harw -x 160 -y 45   # rotate: windows re-anchor, no restart of agents/jobs
tmux resize-window -t harw -x 100 -y 30   # shallow landscape
tmux resize-window -t harw -x 39 -y 60    # compact summary
# Start a background orchestrator and a job (/agent …, /jobs …), verify: three windows + LIVE,
# F4 cycle, Alt+J, x-stop dialog → /jobs stop admission, Alt+L detail, scrolling isolation.

# DoD (on a host with the suite installed; otherwise the "not installed" paths)
harw dod status --json; harw dod doctor; harw dod findings -n 5; harw dod sensors
harw chat   # then: "/dod status", and ask the model about host posture → observe `skills.triggered`
            # fragment in the session export and the dod.* tool calls in the transcript

# Triggers
harw skill preview "cargo test fails with E0505 in src/lib.rs" --command "cargo test"
harw tool list dod; harw tool describe dod.findings

# Execution chain (Linux host with bwrap, Landlock ≥ ABI V4, cgroup v2 delegation)
systemd-run --user --scope -p Delegate=yes -- harw verify-sandbox --backend bwrap-trampoline
#   expect: namespace=Enforced landlock=Enforced(abi N) caps=Enforced nnp=Enforced cgroup=Enforced
harw dod config suggest            # paste into /etc/harw-dod/config.toml, `harw dod enable`
harw chat  → run `!sleep 30` and `shell.exec` via the model; in another shell:
  cat /sys/fs/cgroup/<R>/harw-job-*/cgroup.procs      # the sh and sleep live there
  harw dod findings -n 3; harw dod sensors               # cgroup rows show harw-job-*
```
Attach screenshots (or `tmux capture-pane -p`) for 46×78 and 160×45 to the PR.

### 5.3 Definition of done
- All WPs merged on the branch, gates green at the final SHA, draft PR against
  `dev` with the security section filled (trust boundary: new read-only file
  reads of DoD exports; fail-closed on unreadable/untrusted config; persisted:
  `injection` events in the session store, `health.json`/`findings.jsonl` by the
  sentinel).
- `harw dod status` works on a host without DoD (reports not installed) and with
  it; models can call `dod.*` and receive the `harw-dod` skill on first use.
- Portrait and landscape screens match §3.3.1; every window has its own border,
  focus, scroll and selection; nothing in the TUI calls `JobManager::stop` directly.
- Planning docs relabelled; ledger entries added.

---

## 6. Branch salvage — unmerged work worth porting (all branches fetched as `origin/<name>`)

`origin/main` is an ancestor of `origin/dev` except `a747cee` (#147, container
wall-time fix, no TUI/DoD content) — main has nothing dev lacks. Port by
**cherry-picking hunks, never by merging** these stale branches (they are
280–590 commits behind dev).

### 6.1 TUI
| Source | What | On dev? | Port into |
|---|---|---|---|
| `feat/tui-activity-rendering` (PR #76, `history_cell.rs`, `app.rs`) | `ToolVerbosity::Activity` single-line tool activity (default) + `/verbose` toggle Activity↔Verbose; `split_at_soft_breaks` width-aware wrapping | **No** (dev `ToolVerbosity` is `Compact \| Verbose`, `history_cell.rs:~1563`) | **M3** (fits the activity cells; keep `Compact` as default unless the owner wants `Activity`; skip the Cargo version-pin hunk) |
| `ccr-163653c3-ezvkn6` (3 commits; `hard_kill.rs`, `process_tree.rs`, `input_reader.rs`, `keybindings.rs`, `runtime_root.rs`) | 2×Ctrl+C emergency stop: kills the agent process tree, exits 130; distinguishes background jobs from job-backed children | **No** (dev has only the 2 s quit hint) | **C4b** (new WP after C4; own commit; review deps in its Cargo.toml hunk first; route the kill through `JobManager`/cgroup kill from WP E, never raw PID trees) |
| `chatgpt/async-job-runtime-default` TUI slice (commits `7f526e06`, `1b42d666`) | replay early reasoning on async attach (`reasoning_stream_before_attach_is_replayed`), restore alt-screen after host work | **Partly** (surface_root/`push_pending_event` exist) | **C2** (small cherry-pick; drop runtime-dependent parts) |
| `arch/unify-process-runtime`, `consolidate-merge-0.9.1`, `ccr-9d1aabdf-v191ts` (as a whole) | — | already on dev / superset of the above | nothing |

### 6.2 Plans, activity, macros, background agents — nothing to port
Verified byte-identical or superseded on dev: `chatgpt/semantic-activity-patterns`
(#68) and `coop/pr68-activity-patterns` (#86; dev carries the reviewed 02/03/07
docs with the determinism rules and the `compact.failed == sum(constituent.failed)`
invariant — **M1 must implement those**), `coop/pr81-control-deck` (#84) and
`chatgpt/tui-control-deck-v2` (#81), `docs/pl94-adaptive-tui-progress-20261008`
(#137), `docs/tool-skill-plan` (#124). `copilot/fix-blocking-handoff` (#111) is
dead on dev (delegation is background-only since `c6de7ea8`; dev refuses
`background:false`/`wait:true` via `background_only_refusal()` in
`harw-core/src/turn_loop.rs`). Only `feat/worker-builder-p2` (#74/#78/#85:
`[builder]` config, `harw-job-executor-podman`, `harw worker` CLI, ~1k lines)
is real unported code — **out of this plan's scope**; dev has
`harw-job-executor-oci` instead; needs an owner decision later. Never port the
stray root `uia-mailbox.md`.

### 6.3 Tools / triggers / DoD branches
| Source | What | On dev? | Action |
|---|---|---|---|
| `feat/common-command-tools` (#127), `coop/tool-index` (#89), `feat/skill-triggers` (#126), `pr/obsidian-vault-tools` (#130), `coop/tools-gap` (#88) code | PL-93 crates, `tool_index.rs`, `triggers.toml` engine, obsidian tools, tunnel policy cores | **Yes** (rebased under other SHAs; dev is a superset incl. `harw-tool-cargo/src/guard.rs`) | nothing to port; **use `pr/obsidian-vault-tools`' registration as the template** for B4/D3: `provider!(OBSIDIAN, "obsidian", "harw-tool-obsidian", "tool-obsidian")` (`capability_catalog.rs:~202`), `row!` rows with the write tool as `Write` (~344–349), read-only entries in `AUTO_APPROVED_TOOLS` (`lib.rs:~217–225`), `OBSIDIAN_READ_TOOLS` in `profile.rs` + `authority.rs`, parity test (~638) |
| `ccr-9d1aabdf-v191ts` | `job.wait` in `AUTO_APPROVED_TOOLS`; `harw worker` CLI | deliberately **not** on dev (`harw-tool-job/src/tools/tests.rs:103` asserts no `job.wait` executor; `embedded_agents.rs:3844`) | do **not** port |
| `coop/tools-gap` | `docs/planning/87-tool-gaps/README.md` (PL-87 gap report), `.cargo/config.toml` `xtask` alias | **No** | port the README in **X1** (docs); alias only if it does not collide with the Makefile |
| `ccr-29d4325b-884ntu` (#133 "Translate docs and config comments to English 1/4", commit `571e8aa9`) | translates 30 `dod/` + 34 `docs/` files (German → English); dev still has the German text in all 64 (≈32 changed since) | **No** | **X1b** (optional, docs-only, own commit): re-apply the translation for the DoD files touched by D1 at minimum (`dod/config.toml`, `dod/README.md`, `dod/packaging/config.example.toml`); the rest is a separate follow-up PR |
| main vs dev | nothing DoD/registry/CLI that dev lacks (28 differing files are older variants); one unverified string: main's `job_worker.rs:1167` "prompt job exceeded its token budget" | — | ignore |

**Caution for the executor:** two sweeps disagreed on whether `harw-tool-sys` /
`-gitread` / `-fsread` are registered on dev. The detailed trace (§2.2) found
only `cargo.test_one` wired and the PL-93 README says "nicht an Registry/Profile
angebunden". **Verify with `grep -rn "SysToolProvider\|GitReadToolProvider\|FsReadToolProvider" harw-registry-defaults/src`
before starting B4** and scale B4 down to whatever is actually missing.

## 7. Decisions taken (and what was explicitly left out)

- Suite name **Harw-DoD** in text only; crates/paths unchanged (§3.1).
- Model-facing DoD tools are **read-only**; no harw→warden path until PL-96 W01
  (v2 proof) lands. Human enforcement toggles via `harw dod warden …` (root).
- W02 chat-first is the primary TUI target; the PL-94 model-backed Progress
  Agent, progress bar and todo synthesis are **not** built.
- Keys: `x` stops a job in the Jobs window (not `k`, which is "up" everywhere
  else in this TUI); `Alt+J`/`Alt+F`/`Alt+L` are rebindable `KeyAction`s.
- Execution chain: compose bwrap **around** the trampoline (namespace outside,
  Landlock inside) rather than porting Landlock into `harw-sandbox`; cgroup
  attach stays outside + in-namespace verification; `shell.exec` defaults to
  the runtime path with an explicit `direct` fallback; no seccomp.
- PL-93 internal tool crates get registered (B4) because "internal functions
  the models know how to use" is the ask; scale B4 to whatever is still
  unregistered on dev (§6.3 caution).
- B4 deviates from §3.2 in two places: `sys.*` stays `Full`-only and asks
  (it declares `ExecuteProcess`; fail-closed, same as `process.list`), and the
  `cargo.*` registration in `Full` is deferred, so `cargo-tools` has no command
  or error-code triggers yet (§4 B4).
- W01 inline live cells (C5) are **in scope but last** with a go/no-go after
  C1–C4 land; the three windows + LIVE strip (W02) are the primary target.
- Translation pass (#133) is **not** part of this PR beyond the DoD files D1
  touches (X1b optional).
- Out of scope: ratatui 0.30 upgrade, kitty keyboard flags, `insta` snapshots,
  PL-65 remote/compact client, command palette/pager/work-graph panels from the
  old command contract, `harw tool call`, `harw skill import|new|stats`, MCP
  export of the tool registry, Warden v2 migration (PL-96 W01), netlink probe
  binary, Podman worker/builder (#74), OCI/K8s executors, seccomp.

If the owner disagrees with any decision above, change the corresponding WP
before starting it; nothing else in the plan depends on the alternative.
