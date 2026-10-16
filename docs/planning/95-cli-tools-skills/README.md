---
id: PL-95
title: "An effective CLI, an internal tool landscape and skill injection that fires when it matters"
status: proposed
date: 2026-10-05
baseline: dev@ee1bb10
---

# CLI, tool landscape and skill injection (PL-95)

Goal: the same capabilities are reachable from the command line and from the
agent as typed tools; the agent sees only the tools and skills it needs for
the task at hand; and the right skill text reaches the model **at the moment it
matters**, within a budget, with provenance, without the agent having to know
that it should have looked something up.

## 1. What exists (evidence in the code)

| Piece | Where | State |
|---|---|---|
| ~45 top-level `harw` commands, documented in `docs/cli.md` | `harw-cli/src/cli/mod.rs` | works; grammar grew command by command, no shared rules |
| Skill catalog `SkillIndex` (trusted layers + embedded bundle as fallback, disabled skills hide bundled ones, 512 KiB cap, symlink/traversal guard) | `harw-catalog/src/skill_index.rs` | done |
| Pull model: `skills.search {query,limit,offset}` and `skills.load {name,section}` (64 KiB cap, `##` sections, nearest-name hints) | `harw-registry-defaults/src/skill_tools.rs` | done, auto-approved, no sandbox right |
| Push model: skills **bound in the agent definition** become instruction fragments with SHA-256 provenance at assembly time; a one-line "N skills available" hint | `harw-runtime/src/assembly.rs`, `children.rs` | done, static |
| 75 bundled skills (`harw-home/assets/skills/*/skill.toml` + `instructions.md`: name, description, declared tools/mcps) | `harw-home` | done; manifests have no trigger information |
| Skill proposals with an author ceiling | `skill_proposal_tools.rs` | done |
| Registry-derived tool index | PR #89 (`coop/tool-index`) | open |
| Tool gap report | PR #88 (`coop/tools-gap`) | open |
| Structured tools for common commands (`ls`, `stat`, `ps`, `cargo`, ...) | branch `feat/common-command-tools` | in progress |

## 2. The gaps

1. **Skills are either always on or never seen.** A bound skill costs its full
   text in every turn; an unbound one is only used if the model decides to call
   `skills.search`. Nothing connects "the model just got a borrow-checker
   error E0505" to the skill `rust-borrow-checker-e0505` that already ships.
2. **Manifests carry no trigger.** Matching is a word score over name and
   description; there is no `when`, no path/glob, tool or error pattern.
3. **No budget.** Bound skills are not weighed against the context window;
   there is no per-turn or per-session cap for injected skill text.
4. **Skills do not know the tools in play.** `tools = [...]` in a manifest is
   text only. When a tool group is first used, its usage guidance is not
   delivered.
5. **No measurement.** The learning loop records outcomes, but nothing says
   which skill was injected, why, and whether tool errors or retries went down.
6. **CLI and tools are two surfaces.** A capability that exists as an agent
   tool often has no command, and the reverse. There is no `harw tool` to call
   or inspect a tool, no `harw skill` to list/validate/preview, and the 45
   commands do not share flag, output (`--json`) or exit-code rules.
7. **Authoring and import.** Writing a skill means hand-writing a TOML and a
   markdown file; skills written for other tools (the `SKILL.md` format with
   front matter, as used by Claude Code and Mistral Vibe) cannot be used.

## 3. Design

### 3.1 Three levels of skill presence

| Level | What the model sees | Cost | When |
|---|---|---|---|
| L0 catalog | one line per skill: name + `when` (<= 120 chars) | ~25 tokens each, hard budget (default 1.5 KiB) | always, only the top-ranked N for this agent and task |
| L1 injected excerpt | the best matching `##` section (<= 4 KiB), framed `# Skill: <name> (sha256 ...) [triggered by: ...]` | per turn cap 4 KiB, per session cap 16 KiB | when a deterministic trigger fires |
| L2 full text | the whole skill (<= 64 KiB) | on demand | `skills.load` (exists) or a bound skill |

L0 replaces today's single "N skills available" line, so the model can see
*which* skills exist for its task. L1 is the new part.

### 3.2 Triggers (deterministic first, no extra model call)

Manifest additions, all optional so existing skills keep working:

```toml
when = "Rust borrow checker errors E0499/E0502/E0505"   # one sentence, the L0 line
[triggers]
keywords   = ["borrow checker", "cannot move out"]       # in the task text
error_codes = ["E0505", "E0499", "E0502"]                # in tool output (compiler, tests)
paths      = ["**/*.rs"]                                  # a file the agent reads or edits
tools      = ["process.execute"]                          # first use of a tool in the session
commands   = ["cargo test", "cargo clippy"]               # a command the agent runs
priority   = 50
```

A trigger event (task text arrives, tool result arrives, tool used for the
first time) is matched against an index built once per assembly. Result: a
ranked list of (skill, section, reason). The injector applies:

- **dedupe**: a skill/section is injected at most once per session (again only
  after a compaction that dropped it);
- **budget**: per-turn and per-session byte caps, lowest priority dropped first
  and reported;
- **section selection**: the `##` section whose heading best matches the
  trigger; whole skill only if it is below the per-turn cap;
- **framing and provenance**: same `# Skill:` frame and SHA-256 as bound
  skills, plus the reason, so a reviewer can see why it was there;
- **placement**: as a context fragment next to other instruction fragments,
  never inside user text or tool output;
- **rights**: a skill never grants any (unchanged); skills from an untrusted
  project layer only load after the project is trusted (unchanged).

An optional second stage (model-ranked selection among the deterministic
candidates) is possible later, behind a config switch, and is not part of the
first cycles.

### 3.3 Tools bring their skill

Each tool crate may ship a short usage skill (when to use `fs.read` vs
`search`, how to read the structured `cargo` summary, limits and truncation
flags). The tool registry records the link tool -> skill, and the `tools`
trigger delivers the "how" section the first time the tool is used. The
common-command tool family is the first user.

### 3.4 One landscape for CLI and agent

- **Registry-derived index** (PR #89): the single source of truth for tool
  name, description, argument schema, permission class, group, and linked
  skill. `docs/cli.md`, shell completions and `harw tool list` are generated
  from it, so docs cannot drift (a bundled `cli-doc-drift-review` skill already
  exists for the manual version of this).
- `harw tool list|describe <name>|call <name> --json '{...}'`: call any tool
  from the shell through the same sandbox/permission path as the agent.
- `harw skill list|search|show|preview <task text>|validate|new|stats`:
  `preview` prints what would be injected for a given task and budget, which
  makes triggers testable without a model.
- **CLI rules** applied to all commands: `noun verb`, `--json` everywhere with
  a stable schema, stable exit codes (0 ok, 1 failure, 2 usage, 3 refused by
  policy), no prompts when stdin is not a terminal, aliases kept for the old
  spellings. A test walks the clap tree and fails on a command without
  `--json` or a missing help text.
- **Tool search for the model**: tools are grouped; a small always-on set plus
  a `tools.search` for the rest, so the schema list does not grow with every
  new tool. Group membership comes from the registry, not a hand list.

### 3.5 Measurement

An `injection` event per L1/L2 delivery: skill, section, trigger kind and key,
bytes, turn. The learning loop's outcome tracker joins it with tool errors and
retries after the injection. `harw skill stats` shows per skill: injected,
loaded on demand, later errors of the same kind. A config switch turns L1 off
for A/B comparison. Nothing is sent anywhere; the data stays in the session
store.

### 3.6 Import and authoring

- `harw skill validate`: size, headings, `when` present and short, trigger
  syntax, declared tools exist in the registry.
- `harw skill new <name>`: scaffold with the fields above.
- `harw skill import <dir|file>`: read-only importer for `SKILL.md` with front
  matter (`name`, `description`, optional extras) into a layer skill; imported
  skills are marked as such, get no triggers unless the author adds them, and
  follow the same trust rules as any project layer.

## 4. Cycles

| Cycle | Content | Decision needed |
|---|---|---|
| **S0** | This plan | no |
| **S1** | Optional `triggers.toml` (`when`, `[triggers]`), parser and index in `harw-catalog`; `select(event, budget)` as a pure function with dedupe and budget; unit tests; no runtime wiring | no |
| **S2** | `harw skill list|show|preview|validate` on top of S1 | no |
| **S3** | Wire L0 and L1 into `assemble_round_request` as `Fragment`s (section `skills.triggered`, `Stability::Fresh`), fed by new history items; injection events | after a compaction the dedupe state is reset for dropped items |
| **S4** | Tool -> skill link in the registry; first-use trigger; usage skills for the common-command tools | depends on #89 |
| **S5** | `harw tool list|describe|call` and generated docs/completions | depends on #89 |
| **S6** | CLI rules and the clap-tree test; `--json` gaps closed | which old spellings stay |
| **S7** | Measurement join with the outcome tracker, `harw skill stats`, A/B switch | no |
| **S8** | `harw skill new|import` | accepted import format |
| **S9** | Back-fill triggers for the 75 bundled skills (error codes, paths, tools) | no |

Each cycle: own branch and PR, tests without `unwrap`/`panic`, a mutation check
on every budget and trust check, `xtask gates` green, an honest note on what
ran against a fake provider and what against a real model.

## 5. Decisions for the owner — checked against the code

Each answer below was re-derived from the code on `dev@ee1bb10`, not assumed.

**5.1 Budgets.** Measured on the 75 bundled skills: median 4.7 KiB, p90 8.9 KiB,
max 16.7 KiB; 46 of 75 are larger than 4 KiB; median 5 `##` sections, so one
section is about 1 KiB. Whole-skill injection under a 4 KiB turn cap would
therefore fit only a third of the skills, section injection fits nearly all.
Decision: **inject a section, never the whole skill**, per turn 4 KiB, per
session 16 KiB, L0 1.5 KiB as defaults. The caps must not be a second budget
system: the context assembly already has `ContextBudgetSpec` with per-section
budgets that can only be tightened (`harw-context`), and the 32k fallback window
(`child_controller`) shows small local windows exist. So the skill section gets
its own context section `skills.triggered` with a per-section budget derived from
the window (about 3 % of it, clamped to 1-4 KiB per turn), and the session cap
is the only extra state.

**5.2 L1 on by default?** Where it goes matters more than the switch. The
system prompt and bound skills are `instruction_fragments`, a plain `Vec<String>`
that is part of the cache-stable prefix; changing it mid-session would break
provider prompt caching. Per-turn material goes through `gather_context`, which
runs in `assemble_round_request` **every model round** and yields typed
`harw_context::Fragment`s (trust class, `Stability`, section). So L1 is a
`Fragment` with `Stability::Fresh` in the per-turn context, after the stable
prefix. Trust class: bundled and trusted-layer skills `Instruction` (as bound
skills already are), skills imported from outside `Evidence`. Decision:
**on by default for bundled skills, off for imported ones**, with a config switch.

**5.3 Where the trigger logic lives.** `ContextProvider::contribute` receives only
`TurnInputContext {session_id, turn_id, metadata}`: no history and no tool
results, and `TurnObserver` has only turn start/stop. A provider therefore
cannot see "the compiler just printed E0505". The injector must be core code in
`assemble_round_request` (which has the session and its history), fed by the new
history items since the last round (user text, tool results). S3 is scoped to
that, not to a context-provider plug-in.

**5.4 Manifest compatibility.** `SkillToml` is `#[serde(deny_unknown_fields)]`
(`harw-config/src/skill_toml.rs`) and `SkillIndex` skips a skill whose manifest
does not parse. Adding `when`/`[triggers]` to `skill.toml` would make every skill
that uses them **disappear on an older `harw`**, and the field evidence shows
mixed versions on one tailnet (a thin node with an older binary). Decision:
triggers live in an **optional `triggers.toml` next to `skill.toml`**, which older
builds never read. `when` stays optional there; the L0 line falls back to the
first sentence of `description` (the bundled descriptions already start with
"Use when ...", median 278 characters, so the L0 limit is 160 characters, the
existing `SHORT_DESCRIPTION_CHARS`, not 120). S9 can derive many triggers
mechanically: the descriptions name error codes such as `E0505`.

**5.5 `SKILL.md` import.** The loader today reads `skill.toml` +
`instructions_file` with a 512 KiB cap and symlink/traversal protection; nothing
parses front matter. Import is a pure converter (front matter to `skill.toml`,
body to `instructions.md`) that writes into a layer directory; it adds no new
loading path, so the existing protections apply unchanged. Decision: **yes, as an
explicit `harw skill import`**, never auto-discovered from other tools'
directories.

**5.6 Model-ranked second stage.** No. The deterministic stage has no model call,
which keeps the per-round cost at zero and the result reproducible in
`harw skill preview`. Revisit only if `harw skill stats` (S7) shows many missed
injections.

**5.7 One landscape.** The code already has two layers, and the plan must use
them instead of adding a third: `harw-operations` (`Operation` trait, surfaces
`Command` and `ModelTool`, permission tiers, registry, adapters for the TUI,
channels and model tools; `harw-cli/src/op_bridge.rs::run_operation` already runs
an operation from the CLI) and the `harw-tool-*` provider crates
(`ToolProvider`/`ToolExecutor`, sandbox classes). So `harw tool list|describe`
reads both registries, `harw tool call` goes through `op_bridge` for operations
and through the sandboxed executor path for provider tools. `harw skill ...`
should be an operation with a `Command` surface so the TUI gets `/skill` for
free.

**5.8 CLI rules and old spellings.** `--json` is already a global flag with a
`Printer`; a command without a JSON form calls `Printer::require_text` and fails
loudly. The rule is therefore "every command uses `Printer` or `require_text`",
checked by a test over the clap tree. Old spellings are already governed by
`docs/cli.md` ("Older spellings"): hidden with a note on stderr
(`connect`, `lens`, `uia`, `catalog`, `run`, `classify`, `--bottom-up`) and
permanent aliases (`settings`, `models`, `completion`, `-r`, `HARW_PROFILE`).
Decision: keep that policy as it is; the clap-tree test exempts `hide = true`
commands and lists the permanent aliases explicitly.

**Remaining owner decisions:** (a) the 3 % / 1-4 KiB budget rule, (b) L1 on by
default for bundled skills, (c) the `triggers.toml` file instead of extending
`skill.toml`. Everything else follows from the code.

## 6. Not in scope

Training or fine-tuning on skills; a skill marketplace; letting a skill grant
tool rights.
