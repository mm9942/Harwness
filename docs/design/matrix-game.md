# Matrix Game — Umpire-Led Multiplayer Scenarios with Agents

> Status: implemented · Last reviewed: 2026-09-24

**Scope**: crate `harw-matrix-game` (deterministic game core), the `matrix-game-master` orchestrator role, scenario format `harwness.matrix-scenario/v1`, the read-only `/matrix` panel in `harw-tui`.
**Source material**: J. Curry & T. Price, *Matrix Games for Modern Wargaming* (2014); P. Sabin, *Simulating War* (2012). All content below is summarized in our own words; quotes are at most one sentence, with attribution.

---

## Table of contents

1. [Matrix game format in brief](#1-matrix-game-format-in-brief)
2. [Hidden information & private negotiation](#2-hidden-information--private-negotiation)
3. [Round structure as a state machine](#3-round-structure-as-a-state-machine)
4. [Adjudication in the harness](#4-adjudication-in-the-harness)
5. [Scenario TOML and JSON contracts](#5-scenario-toml-and-json-contracts)
6. [Prompts](#6-prompts)
7. [Orchestration and the TUI](#7-orchestration-and-the-tui)
8. [Lessons from both books](#8-lessons-from-both-books)
9. [Testable invariants](#9-testable-invariants)
10. [Crate layout](#10-crate-layout)
11. [Extensions: behavior, Red Cell, suspicion, precedent, variance](#11-extensions-behavior-red-cell-suspicion-precedent-variance)

---

## Architecture in one paragraph

A **GameMaster** in Rust (`harw-matrix-game`) owns the entire game state, the event journal, the RNG, and the routing decision of *who sees what*. There is no agent-to-agent messaging: every message is a journal event with an `Audience`, and every prompt sent to an agent is built exclusively from the journal's **projection** for that seat. Four player agents (`Seat::Player(p)`) and an umpire agent (`Seat::Umpire`) run as **tool-less child sessions** (multi-turn via `ChildController::run_child`, in parallel via `run_children`). Game runs are started and driven by a dedicated orchestrator role, **`matrix-game-master`** (see §7), not directly by the human at a keyboard; the human is a **facilitator/observer** who watches the run in the TUI and approves each stage. LLM output is the only non-deterministic input; it is journaled, so a replay without model calls reproduces the same state.

```
                 ┌────────────────────── harw-matrix-game ──────────────────────┐
Scenario.toml → │ Scenario ─► GameMaster (phase FSM) ─► Journal (append-only)  │
                 │                 │   ▲                    │                    │
                 │   project(seat) │   │ validated JSON     │ project(Observer)  │
                 └─────────────────┼───┼────────────────────┼────────────────────┘
                                   ▼   │                    ▼
                  run_child(P1..P4, Umpire)             harw-tui `/matrix` panel
                  (tool-less child sessions)            (read-only observer view)
                                   ▲
                                   │ matrix.start / matrix.run / matrix.finish
                          matrix-game-master (harw-ops/src/matrix/game_master.rs)
```

---

## 1. Matrix game format in brief

### 1.1 Core idea

A matrix game produces a plausible narrative of a situation by having players take turns putting forward **arguments**: *something happens* plus *reasons why/how*. An umpire estimates the probability; dice are rolled only where risk exists. A successful argument becomes part of the game world and persists until another argument ends it. Curry & Price sum it up: *"If you can say, 'This happens, for the following reasons...', you can play a Matrix Game."* (Curry & Price, Introduction). The format originates with Chris Engle (1988–1992) and builds on the idea that a thesis and an antithesis combine into a synthesis.

Important for us: it is **not a debate**. Arguments are short and declarative, and the umpire decides quickly so the narrative keeps moving. That fits LLM agents well — short structured contributions instead of long discussion loops.

### 1.2 Variants from Curry & Price and how we map them

| Variant (book) | Content (our summary) | Implementation in the harness |
|---|---|---|
| **Three Reasons** | Action + exactly three reasons; the umpire judges by plausibility, precedent, experience, often weighing the other players' reactions. Other players do *not* interject. | `rules.argument_system = "three_reasons"`: the player supplies `reasons` (exactly 3). The *counter-arguments* phase is skipped; the umpire judges alone. |
| **Pros and Cons** (the authors' recommendation) | Action + pro reasons; the other players give con reasons; the umpire weighs them (strong reasons count double, trivial or repeated ones not at all). Side effect: the cons supply ready-made explanations for *why* an argument fails. | **Default**, `"pros_cons"`. Counter-arguments phase: all non-arguing players submit cons in parallel. The umpire assigns weights 0/1/2 per pro/con; Rust computes the target value (§4). |
| **Simple Narrative** | The player just narrates what happens next; the umpire or a co-player sets the odds. Very accessible but inconsistent. | Not implemented (consistency suffers, especially with LLMs). |
| **Dice & base chance** | Pros/cons originally used 3d6 with a 50% base; the authors prefer a narrative bias: 7+ on 2d6 (≈58%), each net point shifting it noticeably — rewarding a few good reasons over many weak ones. | 2d6, base 7+, each net point shifts the target by 1, bounds in §4.3. |
| **No roll for a compelling argument** | If an argument is convincing, it often needs no roll at all. | `no_roll` is only allowed when net ≥ `rules.auto_success_net` (default 5) and `rules.allow_auto_success = true`. |
| **A very bad roll always fails** | Nothing is certain; a failure means "not now," not "never." | Snake eyes (natural 2) always fails; the target-value ceiling keeps P(success) ≤ 97.2%. |
| **Veto against trivial/nonsensical arguments** | The umpire declines and offers a chance to rephrase. | Umpire field `verdict = "veto"` with a rationale → the GameMaster reprompts the player **once** (with the veto reason); after that the turn is forfeit. |
| **Round consensus** | In the Falklands example, the umpire checks a disputed reason by polling the table; a nod is enough. | Option `rules.consensus_check = true`: for a reason marked `"disputed": true`, the other players' cons count as a straw poll (number of players who explicitly contest it). The book does not describe formal voting — deliberately left out here. |
| **Open reasoning** | The umpire lays out the verdict openly, "like a judge summing up." | The umpire supplies `public_rationale` per public argument (audience `Public`); weights are public. |
| **Secret arguments** | Submitted in writing to the umpire; publicly only *that* a secret argument exists is announced. Used sparingly, only for concrete plans that must stay hidden across several turns — at most one per game as a rule of thumb. | `secret: true` on the argument; limit `rules.max_secret_arguments_per_seat` (default 1); commitment + disclosure §2.4. |
| **Logical inconsistency** | "It happens" vs. "it doesn't happen": the earlier successful argument stands; reversing it is poor form. Better to subtly build on it ("the attack happens, but poorly coordinated"). | The umpire flags `inconsistent_with`; Rust lets the earlier argument in resolution order stand and rejects the later one (with a note in the seat log), unless the umpire chooses `conflict`. |
| **Conflicts** | When two sides are in direct confrontation, both argue over the outcome; both roll until exactly one succeeds. | `conflicts[]` in the umpire output; Rust deterministically re-rolls opposing pairs until exactly one succeeds (cap 10 repeats, then the larger margin wins). The difference between the rolls becomes the `margin` for the narrative. |
| **Ongoing arguments** | A successful argument (a march on X) continues until another one stops it. | Effect op `ongoing` → entry in `world.ongoing`, applied at round end until an effect `stop_ongoing` removes it. |
| **Big projects** | Large undertakings need several successful arguments — a rule of thumb of at most three, or one event dominates the game. | `world.projects` with `stages ≤ 3`; the umpire may advance a stage by only +1 per argument (enforced by Rust). |
| **Hidden/protected things** | A hidden thing must first be found; a protected thing must be overcome stage by stage. | Scenario objects with `hidden` and `protection = n`; effect ops `discover`, `breach` (−1 stage). Rust rejects actions on objects that are still hidden/protected. |
| **Fail chits** (optional) | A player who fails gets a chit for a later re-roll — a hedge against early bad luck. | `rules.fail_chits = true`; the player sets `use_fail_chit_if_failed` at submission time (no extra round needed). |
| **Turn order** | Either fixed per scenario, or decided by the umpire; whoever leads goes first, so weaker parties get more time to think. | `rules.turn_order = "fixed" \| "leader_first"`; `leader_first` uses the umpire's latest `standing` estimate. Under sealed submission (§3), the order becomes the **resolution priority**. |
| **A shared purpose** | Every game needs a short, clear purpose so nobody strays off topic. | Required field `scenario.purpose`, first line of every prompt. |
| **Comparable role level** | Roles should operate at a similar level (not three generals and a single soldier). | Validation warning when `faction.level` is not uniform. |
| **Goals as short bullets** | Briefings are condensed into a few crisp goals (typically 3). | `goals.public` / `goals.secret` per faction, at most 4 entries each. |
| **Game end via final arguments** | When time runs out, each player describes how it ends (three reasons); everyone rolls together, those who fail drop out until one remains. | Option `rules.ending = "final_arguments"` (default `"fixed_rounds"`), own phase *Final Arguments*. |
| **Hot wash-up / AAR** | The debrief is the real learning outcome; the umpire becomes the seminar leader. Arguments plus pros/cons and the umpire's summary are recorded; contested points can be replayed with alternatives. | AAR document at game end (§8.2); `fork --from-round N` on the journal. |

---

## 2. Hidden information & private negotiation

### 2.1 What Sabin says (condensed)

Sabin distinguishes **direct** simulation of the "fog of war" (players genuinely see less — separate rooms as in classic kriegsspiel, hidden pieces, double-blind maps) from **indirect** simulation (randomness and turn order create uncertainty without distributing information asymmetrically). His warning: direct information asymmetry adds complexity and slows play, makes testing harder (a designer can't playtest alone), and stops an umpire from fixing mistakes without giving away secrets (Sabin, ch. 7). But umpire-run multiplayer games are exactly where direct concealment works, because a neutral party holds the secrets.

**Consequence for us**: we *want* direct concealment (private negotiations, secret goals, secret arguments), but the Rust GameMaster carries the bookkeeping load that Sabin describes as the cost, and the human facilitator sees everything — so, like Sabin's "guided competition," they can intervene without becoming a leak themselves, since they are not a player. We also use Sabin's indirect tools: dice and sealed simultaneous submission.

### 2.2 Visibility model

```rust
pub enum Seat { Player(PlayerId), Umpire }

pub enum Audience {
    Public,                          // all seats
    Pair(PlayerId, PlayerId),        // exactly these two players (normalized: a < b)
    UmpireOnly,                      // umpire only
    Seat(PlayerId),                  // exactly one player (e.g. secret goals, own notes)
    SeatAndUmpire(PlayerId),         // player + umpire (secret argument and its verdict)
}
// The human (observer/facilitator) is not a Seat and always sees everything.

fn visible_to(aud: &Audience, seat: &Seat, cfg: &VisibilityCfg, ev: &Event) -> bool;
fn project(journal: &Journal, seat: &Seat, cfg: &VisibilityCfg) -> SeatView; // sole input for prompts
```

- `SeatView` is a newtype; the prompt builder accepts **only** `&SeatView`, never `&Journal` or `&WorldState`. That makes "players only know their own projection" a type-level property.
- World variables carry their own visibility (`public`, `umpire`, `seat:<id>`, `seats:[..]`); `project` filters them the same way.
- Channel IDs for pairs are opaque (`neg-7f3a`), not derivable from seat names.

### 2.3 Private negotiations

Requirement: uninvolved parties learn **neither the content nor the existence** of a conversation.

1. **Request**: in the *Negotiation* phase, each player (in parallel, response audience `Seat(p)`) submits a list of negotiation requests with an opening message — or none.
2. **Opening**: Rust opens a channel (`Audience::Pair`) for each requested pair. The requested player sees the opening in their next prompt; declining means not responding or sending `decline`.
3. **Exchange**: up to `rules.negotiation.max_exchanges` rounds (default 2). Per exchange, each player gets *one* call covering all their open channels; players without a channel are not called. The number of calls a seat gets depends only on its own channels.
4. **Deals** (`proposal`/`accept`) are stored as a `Pair` fact but are **not binding** (Rust enforces nothing). Betrayal is part of the game.
5. **The public log** contains only "negotiation phase ended" — no count, duration, or participants.

**Umpire access** to negotiations, scenario option `visibility.umpire_negotiations`:

| Value | Meaning | Use |
|---|---|---|
| `"none"` (**default**) | The umpire sees no pair channels. If a player publicly invokes a deal, that is their own disclosure. | Minimizes the leak surface: umpire text is all public. |
| `"cited"` | The umpire sees a channel only if a player references it in an argument via `cites_negotiation` — then as `UmpireOnly` context for exactly that adjudication. | Deals become usable as "reasons" without standing access. |
| `"full"` | The umpire sees all channels (like a game master listening in at the table). | Only with the leak guard active (§2.5). |

A distinction that applies throughout: a **system leak** (the harness passes on something a seat isn't meant to see) is a bug. **In-game disclosure** (player A tells C publicly or privately what B said) is legitimate play and is marked in the journal as `disclosed_by: A`.

### 2.4 Secret arguments

- **Submission**: an argument with `secret: true`, audience `SeatAndUmpire(p)`. Rust checks the limit (`max_secret_arguments_per_seat`) and the book's rule "only for things that must be planned ahead" as a prompt instruction to the umpire (who may reject with `veto` if it should have been made openly).
- **Commitment**: publicly, "player *X* submits a secret argument (#s3)" appears, plus `sha256(canonical JSON ‖ salt)`. That text is a **Rust template**, never umpire prose.
- **No counter-arguments** from other players (they don't know the content); the umpire supplies the cons itself (`umpire_cons`).
- **Roll**: normal, deterministic. Publicly: only "#s3 has been resolved" (the outcome is optionally public via `visibility.secret_outcome_public`, default `false`).
- **Effects** may only change variables whose visibility is ⊆ {`umpire`, `seat:<owner>`}, or must be marked `deferred`. Deferred effects are applied only on disclosure.
- **Disclosure** (`reveal`) happens: (a) when the condition `reveal_when` triggers (e.g. another argument touches the object; the umpire reports `triggers_secret: "s3"`), (b) when the owner reveals it in a later argument, (c) on facilitator command, (d) **always**, at the latest, at game end in the AAR. On disclosure, content, salt, and commitment are published — anyone can check the hash.

### 2.5 Leak guards (defense in depth)

1. **Structural**: prompts are built only from `SeatView` (a type-system guarantee, see above). Every child session receives only its own prompts — its transcript can never contain more than its projection.
2. **Session isolation**: one child per seat, no shared sessions, no tools (so no file access to the journal or scenario either).
3. **Templates for sensitive public lines**: announcements of secret arguments, phase changes, end of negotiation — all Rust-generated text.
4. **Content scanner** on everything an umpire outputs as `Public` (especially under `full`/`cited`): shingle overlap (5-word n-grams) against undisclosed `Pair`, `Seat`, and secret content, plus mentions of secret world-variable names/values. A hit → a reprompt with a hint; a second hit → the text is withheld and the facilitator gets a `LeakSuspect` to approve or edit.
5. **Player scanner**, flagging only: if a player publicly quotes content from their own pair channel, it's marked as in-game disclosure, not blocked. If they quote content they couldn't possibly know, that's a system-leak alarm (test invariant).
6. **No metadata leak**: public events carry no timestamps or sequence numbers from the negotiation phase; round-log numbering is local per audience.

---

## 3. Round structure as a state machine

```
Setup ─► Briefing ─► Negotiation ─► Arguments ─► Counter-arguments ─► Adjudication ─► Publication ─► Round end
            ▲                                                                                          │
            └────────────────────────────── next round (situation update) ◄──────────────────────────┤
                                                                                                        ▼
                                                                     [Final arguments] ─► Game end/AAR
```

Every transition is a journal event `PhaseEntered{round, phase}`. The GameMaster runs a fixed set of calls per phase; facilitator commands (§7) are applied only at phase boundaries (except `pause`).

| Phase | Who acts | Input (projection) | Output (validated) | Journal effect / audience |
|---|---|---|---|---|
| **Setup** | Rust | Scenario TOML, seed | `WorldState₀`, seat assignment, child sessions | `GameCreated` (observer), commitment to the scenario hash |
| **Briefing** (round 1: full; afterward "situation update") | all 4 players + umpire (parallel) | Player: purpose, public situation, own faction, public + **own** secret goals, own hidden variables. Umpire: everything except pair channels (per option) | Player: `ack` + short private intent (`intent`, audience `Seat(p)`) — used only for the AAR | `Briefed{seat}` |
| **Negotiation** | players with a request/channel | own channels + situation | `negotiation_request[]`, then `negotiation_message[]` per exchange | `Pair` events; publicly only `NegotiationClosed` |
| **Arguments** | all 4 players, **sealed, parallel** | situation, own channels, ongoing effects, own fail chits, secret quota | `player_argument` | Immediately `ArgumentSealed{id, commitment}`; content revealed only after *all four* submissions arrive → `ArgumentsRevealed` (`Public` or `SeatAndUmpire`) |
| **Counter-arguments** (`pros_cons` only) | each player once, for all other players' public arguments (parallel) | all public arguments of the round | `counter_argument` (list of cons per argument ID) | `CountersSubmitted` `Public` |
| **Adjudication** | umpire (2 calls) + Rust | Call A: all arguments, cons, situation, channels where applicable. Call B: dice results | A: `umpire_adjudication` (weights, modifier, effects *for both success and failure*, conflicts, vetoes). Rust: validation → target values → rolls. B: `umpire_narration` | `Adjudicated`, `DiceRolled` (public for public arguments), `Narrated` |
| **Publication** | Rust | outcome branches | applied deltas | `WorldDelta` per variable with its visibility; `PublicSummary` |
| **Round end** | Rust (+ umpire optionally supplies `standing`) | world state | apply `ongoing`, check disclosure triggers, check end condition, snapshot | `RoundClosed{round, state_hash}` |
| **Final arguments** (optional) | all players, then umpire, then Rust | overall situation | each player submits a closing argument (3 reasons); umpire → target values; Rust → knock-out rolls | `FinalOutcome` public |
| **Game end/AAR** | umpire + optionally players (debrief) | **full disclosure** (all audiences become public) | `aar_input` per seat, umpire synthesis | AAR markdown (§8.2) |

**Why sealed instead of round-robin?** Curry & Price play arguments in sequence. But Sabin shows that when one side decides after the other, the later party has a substantial information advantage (ch. 7). Sealed submission removes that, allows parallel LLM calls, and still keeps order as the resolution priority for inconsistencies. Option `rules.argument_mode = "sequential"` remains for scenarios where reacting to the previous argument is desired (player *n* then sees the published arguments 1..n−1 of that round).

**Round count**: default 6, 6–8 recommended per scenario — Curry & Price report that fewer than six turns don't let themes mature, and too many become repetitive (Lasgah Pol). Optional "pre-round" (pre-deployment) and "post-round" (what happens afterward) via `rules.prologue`/`rules.epilogue`.

**Error paths**: invalid JSON → one reprompt with the parser error; a second failure → the seat "passes" for that phase (`Forfeit{seat, phase}` public, as "player X submits no argument"). Umpire error → the phase pauses, the facilitator decides (retry / manual adjudication).

---

## 4. Adjudication in the harness

### 4.1 Division of labor

| Step | Who | Why |
|---|---|---|
| Judge reasons, context modifier, inconsistencies, conflicts, vetoes | **Umpire (LLM)** | Judgment — this is the core of the format. |
| Fix effects for **both success and failure in advance** | **Umpire (LLM)** | Prevents consequences from being fitted to the narrative after the roll. |
| Weights → target value → probability | **Rust** | A uniform, checkable mapping; the umpire can't set arbitrary numbers. |
| Roll the dice | **Rust**, seeded, journaled | Determinism, replay. |
| Apply the delta, enforce bounds | **Rust** | World state is never LLM-written. |
| Narrate the outcome | **Umpire (LLM)**, call B | Prose only, no state change is possible. |

### 4.2 Weighting scheme (default `pros_cons_2d6`)

- Every pro reason and every con reason gets a weight `w ∈ {0, 1, 2}` from the umpire:
  `0` = trivial, a mere restatement of an already-counted reason, or factually wrong; `1` = sound; `2` = compelling.
- Only the **three highest-weighted pros and three cons** count (against "laundry lists").
- Context modifier `m ∈ [−2, +2]` with a mandatory rationale (inherent probability, situation, precedent — e.g. in the Falklands example, a point of international law that no player had raised).
- `net = Σ top3(pro) − Σ top3(con) + m`, so `net ∈ [−8, +8]`.
- `target = clamp(7 − net, 3, 11)`; success if `2d6 ≥ target` **and** the roll ≠ 2 (snake eyes always fails).
- Fail chit (if set and available): exactly one re-roll, the second roll counts.

| target | 3 | 4 | 5 | 6 | **7** | 8 | 9 | 10 | 11 |
|---|---|---|---|---|---|---|---|---|---|
| P(success) | 97.2% | 91.7% | 83.3% | 72.2% | **58.3%** | 41.7% | 27.8% | 16.7% | 8.3% |

Bounds: P(success) ∈ [8.3%; 97.2%]. Nothing is impossible, nothing is certain — except when the umpire proposes `no_roll` and `net ≥ auto_success_net` (default 5) with `allow_auto_success = true`.

**Alternative** `rules.adjudication = "estimative_d100"`: the umpire picks a rung from a fixed ladder (`5, 15, 30, 50, 70, 85, 95`%, worded "almost impossible" … "almost certain") plus a rationale; Rust accepts only ladder values and rolls d100. This variant isn't in Curry & Price but is useful for scenarios aimed at an analyst audience; the default stays 2d6, because the net-point logic keeps the pro/con structure transparent.

**Conflicts**: for a conflict pair (A, B), `target_A` and `target_B` are each determined as above; Rust rolls both, repeating until exactly one succeeds (cap 10, after which the larger margin `roll − target` wins, ties going to the earlier party in resolution order). `margin = |difference|` feeds the narrative ("a narrow win, an orderly retreat is possible" vs. "collapse").

**Degree of outcome**: a roll ≥ target+3 or ≤ target−3 is reported to the narrative as `strong_success`/`strong_failure`. The state effect stays the branch fixed in advance (no extra effects — simplicity over nuance).

### 4.3 Deterministic RNG

- `master_seed: [u8; 32]` comes from the scenario (`seed`) or is drawn randomly at start and **stored as the first journal event**.
- Every roll uses a **derived** seed, independent of call order:
  `sub_seed = SHA-256("harw-matrix/v1" ‖ master_seed ‖ round ‖ roll_kind ‖ arg_id ‖ attempt)` → `ChaCha20Rng::from_seed(sub_seed)` → two rolls, `gen_range(1..=6)`.
  So parallel execution, retries, or a facilitator intervention don't change the other rolls.
- `DiceRolled{arg_id, attempt, dice:[u8;2], target, success, sub_seed_hex}` is journaled; replay checks that recomputing yields identical values (otherwise `ReplayDivergence`).

*(See `harw-matrix-game/src/dice.rs` for the exact seed derivation and `dice::resolve_conflict` for the conflict re-roll loop.)*

### 4.4 World state & delta

Effect operations (proposed by the umpire, validated by Rust):

| Op | Effect | Rust check |
|---|---|---|
| `add {var, by}` | move a track | var exists; `\|by\| ≤ rules.max_track_step` (default 1, up to 2 per scenario); clamp to `[min,max]` |
| `set {var, value}` | set a discrete state | value is in `values` |
| `fact {text, audience}` | narrative fact in `world.facts` | audience ⊆ the argument's audience |
| `ongoing {id, text, each_round:[ops]}` | ongoing effect | ops are themselves valid; max `rules.max_ongoing` (default 6) |
| `stop_ongoing {id}` | ends it | id exists |
| `project_advance {id}` | big project +1 stage | max +1 per argument, `stages ≤ 3` |
| `discover {object}` / `breach {object}` | hidden → known / protection −1 | order: `discover` before `breach` |
| `reveal_secret {secret_id}` | trigger disclosure | secret exists, not yet revealed |

Violations trigger a reprompt of the umpire with the error list (one attempt), after which invalid ops are dropped and shown to the facilitator as `EffectRejected`. The umpire never writes state directly; `state_hash` (SHA-256 over canonical JSON) is journaled at every round end.

Business-mode adds a second family of effects on top of this: rule-based restrictions/vetoes and a market model (§1.6 of `wargaming-and-analysis.md`), implemented in `harw-matrix-game/src/business.rs` — see §7 below.

---

## 5. Scenario TOML and JSON contracts

### 5.1 A complete example scenario (fictional)

```toml
schema = "harwness.matrix-scenario/v1"
id = "karst-water-crisis"
title = "Water Crisis on the Karst Islands"
purpose = "A matrix game about the fight over the only desalination plant on the Karst Islands during a drought summer."
seed = "optional-hex-or-empty"          # empty = drawn at start and journaled
rounds = 6
round_represents = "about two weeks"

[rules]
argument_system = "pros_cons"            # "pros_cons" | "three_reasons"
argument_mode = "simultaneous"           # "simultaneous" (sealed) | "sequential"
adjudication = "pros_cons_2d6"           # "pros_cons_2d6" | "estimative_d100"
turn_order = "fixed"                     # "fixed" | "leader_first"
max_track_step = 1
max_ongoing = 6
allow_auto_success = true
auto_success_net = 5
fail_chits = true
consensus_check = false
max_secret_arguments_per_seat = 1
ending = "fixed_rounds"                  # "fixed_rounds" | "final_arguments"
prologue = false
epilogue = true
debrief_players = true                   # player agents contribute an AAR reflection

[rules.negotiation]
enabled = true
max_exchanges = 2
max_channels_per_seat = 2
max_message_chars = 800

[visibility]
umpire_negotiations = "none"             # "none" | "cited" | "full"
secret_outcome_public = false
leak_guard = "strict"                    # "strict" | "flag_only"

[models]                                 # optional, otherwise the harness default model
umpire = "default"
players = "default"

# ---------- World ----------
[world]
public_situation = """
No rain for six weeks. The desalination plant in the port of Velmar is running at only
60% capacity. The island council has announced rationing; the harbor guild controls
tanker traffic, the Northern Realm is offering "technical assistance," and a mediation
mission from the Sea League has been on site since yesterday.
"""

tracks = [
  { id = "stability",           label = "Public order",                min = -3, max = 3, start = 0,  visibility = "public" },
  { id = "water",                label = "Water supply",                min = -3, max = 3, start = -1, visibility = "public" },
  { id = "north_influence",      label = "Northern Realm influence",    min = 0,  max = 5, start = 1,  visibility = "public" },
  { id = "rat_support",          label = "Council's public support",    min = -3, max = 3, start = 1,  visibility = "public" },
  { id = "smuggling_net",        label = "Guild's smuggling network",   min = 0,  max = 3, start = 2,  visibility = "seat:guild" },
  { id = "north_agents",         label = "Northern Realm agents in port", min = 0, max = 3, start = 1,  visibility = "seat:north" },
  { id = "plant_sabotage_risk",  label = "Sabotage risk at the plant",  min = 0,  max = 3, start = 1,  visibility = "umpire" },
]
states = [
  { id = "plant_control", label = "Control of the desalination plant", values = ["council", "guild", "north", "mission", "contested"], start = "council", visibility = "public" },
]
objects = [
  # only the umpire knows about this reservoir at start; hidden, 1 protection stage
  { id = "reservoir_oldmark", label = "The old Oldmark reservoir", hidden = true, protection = 1, visibility_when_found = "public" },
]
projects = [
  { id = "pipeline", label = "Emergency pipeline from the mainland", stages = 3, progress = 0, visibility = "public" },
]

# ---------- Factions (exactly 4 player seats) ----------
[[factions]]
id = "council"
name = "Island Council"
level = "Government"
briefing = "Elected government, tight budget, elections in three months."
goals.public = ["Keep public order from dropping below 0", "Retain control of the plant"]
goals.secret = ["Make the guild look responsible for the crisis"]
assets = ["Island police", "Rationing authority"]

[[factions]]
id = "guild"
name = "Harbor Guild"
level = "Economic power"
briefing = "Controls tankers, cranes, and the dockworkers."
goals.public = ["Secure tanker licenses", "Rationing that favors the port"]
goals.secret = ["Expand the smuggling network (smuggling_net = 3)", "Gain control of the plant"]
assets = ["Tanker fleet", "Dockworkers' syndicate"]

[[factions]]
id = "north"
name = "Northern Realm"
level = "Neighboring state"
briefing = "A large neighbor with surplus water and political ambitions."
goals.public = ["Be seen as a helper in a crisis"]
goals.secret = ["north_influence at least 4 by game end", "Prevent the pipeline project"]
assets = ["Tankers", "Technicians", "Embassy"]

[[factions]]
id = "mission"
name = "Sea League Mediation Mission"
level = "International organization"
briefing = "A small team with a mandate but no power to enforce it."
goals.public = ["Prevent escalation", "Get the pipeline to at least stage 2"]
goals.secret = ["Position the organization as indispensable"]
assets = ["Mandate", "Donor-conference contacts"]

[seating]                                  # seat → faction, order = turn order
order = ["council", "guild", "north", "mission"]

[[injects]]                                # prepared facilitator events (optional)
id = "heatwave"
round = 3
audience = "public"
text = "A heatwave doubles water demand."
effects = [{ op = "add", var = "water", by = -1 }]
```

**Validation on load**: exactly 4 factions; unique IDs; `start ∈ [min,max]`; visibilities reference existing factions; `stages ≤ 3`; `purpose` not empty; a warning when `level` is inconsistent; at most 4 goals per category.

### 5.2 JSON contracts

All responses are **one** JSON object, validated via Serde (`deny_unknown_fields`) plus length limits. The GameMaster assigns IDs; agents only reference IDs that appear in their own projection (otherwise a validation error).

**negotiation_request** (player, negotiation phase, step 1)
```json
{ "requests": [ { "to": "north", "opening": "We should talk about tanker licenses before the council does." } ] }
```

**negotiation_message** (player, per exchange; one entry per open channel)
```json
{
  "messages": [
    { "channel": "neg-7f3a", "text": "In exchange for 2 tankers of water per week, we'll publicly support your aid offer.",
      "proposal": { "summary": "2 tankers/week for public support" }, "accept": null, "decline": false }
  ]
}
```

**player_argument**
```json
{
  "action": "The guild takes over operation of the desalination plant on the council's behalf.",
  "pros": ["The guild has the only technicians with harbor-crane access.",
           "The council has no budget for overtime.",
           "The dockworkers would otherwise strike."],
  "secret": false,
  "cites_negotiation": [],
  "conflict_target": null,
  "project": null,
  "use_fail_chit_if_failed": false,
  "private_note": "First step toward controlling the plant."
}
```
`private_note` has audience `Seat(p)` (only for the AAR and the facilitator). Under `three_reasons`, exactly 3 `pros` are required, otherwise 1–5.

**counter_argument**
```json
{
  "counters": [
    { "argument_id": "r2-a1", "cons": ["The council would be giving up its most important power base.",
                                         "The police already guard the plant."] },
    { "argument_id": "r2-a3", "cons": [] }
  ]
}
```
Only argument IDs of other players' public arguments from the current round; at most 3 cons per argument.

**umpire_adjudication** (call A, before the roll)
```json
{
  "rulings": [
    {
      "argument_id": "r2-a1",
      "verdict": "roll",
      "pro_weights": [2, 1, 1],
      "con_weights": { "council": [2], "mission": [1] },
      "umpire_cons": [],
      "context_modifier": -1,
      "context_reason": "A government rarely gives up critical infrastructure voluntarily.",
      "inconsistent_with": null,
      "public_rationale": "Technically plausible, politically costly for the council; the strike threat weighs heavily.",
      "private_notes": "If successful, sabotage risk does not rise internally.",
      "on_success": [ { "op": "set", "var": "plant_control", "value": "guild" },
                      { "op": "add", "var": "rat_support", "by": -1 } ],
      "on_failure": [ { "op": "fact", "text": "The council refuses; the guild openly threatens a strike.", "audience": "public" } ],
      "triggers_secret": null
    }
  ],
  "conflicts": [],
  "standing": ["guild", "council", "north", "mission"]
}
```
`verdict ∈ {"roll", "no_roll", "veto"}`; `veto` requires `public_rationale` as the rejection reason. `con_weights` is grouped by faction, in the order the cons were submitted. For secret arguments: `umpire_cons` instead of `con_weights`, and `public_rationale` must be `null` (the Rust template replaces it).

**umpire_narration** (call B, after the roll)
```json
{
  "narrations": [
    { "argument_id": "r2-a1", "audience": "public",
      "text": "After tense talks, the council hands over operations; guild technicians move into the plant." }
  ],
  "round_summary": "The guild now controls the water supply; the council loses public support."
}
```
A narration's `audience` may not be broader than the argument's; the leak guard runs over every `public` text.

---

## 6. Prompts

Outlines, not final text. Prompts are in `scenario.language`; the JSON schemas are appended verbatim.

### 6.1 System prompt — player

1. **Role**: "You play the faction *{name}* in a matrix game. Purpose: *{purpose}*." You are an actor, not a narrator or umpire.
2. **What you know**: "You know only what is in your situation report. There are things you don't know — other factions have their own goals and can talk to each other privately without your knowledge. Do not invent knowledge of other factions' secret goals, deals, or hidden values."
3. **Arguing**: one concrete action per turn, at the level of your faction; a few strong reasons rather than many weak ones; nothing that simply reverses an already-successful event — build on it instead. Break large undertakings into steps.
4. **Counter-arguments**: factual reasons why a foreign argument might fail; no repetition, no polemics.
5. **Negotiating**: deals are not binding; you may bluff and break agreements. What's in a private channel stays private unless *you* deliberately disclose it — doing so is itself a move.
6. **Secret arguments**: at most {n} per game, only for concrete preparations that must stay hidden across several turns.
7. **Format**: respond only with a JSON object per schema; no meta-commentary about the game, the model, or the harness.
8. **Anti-leak (self-protection)**: "Only put private-channel content into public fields if you intend to disclose it deliberately; mark that in `private_note`."

### 6.2 System prompt — umpire

1. **Role**: neutral referee and narrator. The goal is a plausible, coherent narrative, not a winner. Favor no faction.
2. **Judging**: check each reason for plausibility, situation, precedent; weights 0/1/2 by a fixed standard; a context modifier only with a rationale and only for factors no player raised. Don't pre-empt what the dice should decide; don't use dice as a substitute for missing judgment (Sabin, after Rubel).
3. **Vetoes**: reject or flag as risky trivial, unrealistic, or game-breaking arguments ("I attack and win the war") — Curry & Price describe exactly this case.
4. **Effects before the roll** for success *and* failure, small and within bounds; explain failures from the cons.
5. **Consistency**: watch prior successful arguments, ongoing effects, and big projects; report inconsistencies instead of silently ignoring them.
6. **Secrecy**: "Whatever you know as UmpireOnly or from secret arguments must not be quoted or hinted at in public text. Public rationales rest only on public knowledge. Never mention private negotiations (even that they took place), even where visible to you."
7. **Narration** (call B): short, concrete, outcome matches the roll's degree (`strong_success` etc.), no new state changes.
8. **AAR mode** (game end): role shifts to seminar leader — patterns, key moments, alternatives, tie back to the purpose.

### 6.3 Per-phase user prompt (structure)

```
[Purpose] … [Round r/R, Phase] …
[Public situation]    ← project(seat).public_world
[Your faction]        ← briefing, goals (public + own secret), own hidden tracks
[Ongoing effects / big projects / known objects]
[Public log since your last turn]   ← delta, not the full history (session is multi-turn)
[Your private channels] (own only)
[This phase's task] + JSON schema
```

Because the child session is multi-turn, each call sends only the **delta** of the projection; at the start of a round, a full "situation update" is sent as well (protection against context drift and a basis for compaction).

---

## 7. Orchestration and the TUI

### 7.1 `matrix-game-master`: the orchestrator that runs games

Matrix games are started and driven by a dedicated orchestrator role, **`matrix-game-master`** (`harw-registry-defaults/agents/matrix-game-master.toml`, role rules in `harw-registry-defaults/knowledge/roles/matrix-game-master.md`). The UIA hands off to it (`transfer_to_matrix-game-master`) as the root of its own background agent tree; it runs as a `root-orchestrator` so the TUI event loop is never blocked while seat agents are working. Its job is purely game mastering: turn a free-text request into a validated scenario, present it for approval, play it out with the four seat roles (started by the runner as its children), and deliver an AAR plus a paper-ready `report.md`. It never rolls dice itself — the engine (`harw-matrix-game`) does.

The role's tool surface is exactly five model-tool operations, defined with `#[operation]` in `harw-ops/src/matrix/game_master.rs` and bound to the `matrix-game-master` role by a role-scoped `ModelToolProvider` (not exposed to the UIA root or any other role):

| Tool | Effect | Approval |
|---|---|---|
| `matrix.draft_scenario` | validates a scenario TOML and saves it under `<profile>/knowledge/matrix/scenarios/<slug>.toml` | none (matrix storage only) |
| `matrix.status` | status of a run, a list, or the source of an example/draft scenario | none (read-only) |
| `matrix.start` | opens a run (setup, run directory) | always required |
| `matrix.run` | plays rounds with the seat agents (cost, budget) | always required |
| `matrix.finish` | final arguments, AAR, `report.md`, copy into the workspace | always required |

Scenarios can be **drafted from free text**: the orchestrator turns the user's request into a scenario draft via `matrix.draft_scenario`, which validates it against the schema and returns a summary for approval before anything is played. The orchestrator itself only reads (`fs.read`, `fs.list`, `fs.search`, `fs.glob`, `fs.grep`, `doc.read_pdf`) and writes nothing outside its own matrix storage and — from `matrix.finish`, itself approval-gated — exactly one new file `<workspace>/matrix/<scenario>-<run>.md` (never overwriting, no symlink target). Each seat call has a hard time limit (`harw-ops/src/matrix/runner.rs::SEAT_TURN_TIMEOUT`), and a `matrix.run` call ends at the next phase boundary within a fixed call budget, so a long-running game never blocks the orchestrator's own turn.

### 7.2 `/matrix`: a read-only observer panel

The `/matrix` slash command in `harw-tui` is **read-only**. It used to also start and step games directly (`start`/`step`/`auto`); that blocked the TUI event loop for as long as the seat agents were running, and approval dialogs for the seats could not appear. Those subcommands now just point the user at the orchestrator instead. The remaining, supported subcommands (`harw-ops/src/matrix/mod.rs`) are:

- `show` (also bare `/matrix`) — panel data for the current run, or the one picked via `--run=<id>`.
- `list` — runs known to this process, plus bundled and saved scenarios.
- `replay` — verifies a run's journal by replaying it (without restarting play).
- `compare <run> <run> …` — compares several runs (design lessons, see §8.1/§11.5).

`show` returns `{"run_id","scenario","round","phase","status","seats":[{"id","name","role"}],"observer":[E],"views":{"<seat_id>":[E]},"channels":[{"id","members":["a","b"]}]}` with `E = {"round","kind","audience","from","text"}`; the per-seat `views` come from `project(journal, seat)` — exactly what that seat sees, the same mechanism as §2. If an `AgentEventHub` is available, every new journal line is also pushed live as a matrix event, so the TUI panel updates as the game plays out under the orchestrator's control.

Actual control of a run — starting it, stepping through rounds, injecting events, resolving conflicts, forking — happens exclusively through the `matrix-game-master` tool calls above, gated by the harness's normal approval flow, not through dedicated facilitator hotkeys in the TUI.

---

## 8. Lessons from both books

### 8.1 What we encode

| Lesson | Source | Encoding |
|---|---|---|
| Keep it simple; a simple game that gets played teaches more than a detailed one that never does. Sabin: *"a simple wargame that is played will be more instructive than a detailed wargame that is not."* | Sabin, ch. 2 | Few tracks (validation warning above 10), at most 4 goals, effect steps of ±1, no combat tables in the first cut. |
| Free kriegsspiel lives and dies by a respected, knowledgeable, impartial umpire; that's why a hybrid with tables emerged. | Sabin, ch. 3 | Hybrid: the LLM judges, Rust enforces the weight scale, bounds, dice. |
| Dice are no substitute for unmodeled reality. | Sabin, ch. 8 (Rubel) | The umpire must justify context explicitly as a modifier; `no_roll` for clear-cut cases. |
| Reality, skill, and chance as a triangle; chance breaks hindsight knowledge but must not outweigh good decisions. | Sabin, ch. 7–8 | 2d6 instead of d6 (bell-shaped), target-value bounds, fail chits as a counterweight. |
| Victory conditions shape behavior as strongly as movement and combat rules. | Sabin, ch. 8 | Public + secret goals, scored in the AAR (0–3 per goal with a rationale); no global winner by default. |
| Multiplayer with individual goals creates cooperation beyond zero-sum (Diplomacy-like); short negotiation windows before decisions, then announcements in fixed order. | Sabin, ch. 7/9 | Negotiation phase before arguments, bounded exchange rounds, fixed resolution order. |
| Direct fog of war costs complexity and testability. | Sabin, ch. 7 | Bookkeeping in Rust; projections covered by property tests; the human sees everything. |
| Designs are never finished, only abandoned (Vasey via Sabin); play repeatedly to see the spread. | Sabin, ch. 8 | Batch mode via `matrix compare` across multiple seeds/runs, evaluating end states. |
| Validation: does historical behavior roughly reproduce the known course of events? Do rational players sometimes choose the real strategies? | Sabin, ch. 8 | AAR section "plausibility check" with exactly these questions, answered by the umpire. |
| Matrix games don't predict the future; their value lies in participation and insight. | Curry & Price, Introduction | Note at the top of the AAR; no "forecast" language in prompts. |
| A few good reasons beat a laundry list; trivial repeats don't count. | Curry & Price, Pros and Cons | Top-3 rule, weight 0. |
| Record arguments, pros/cons, and the umpire's summary; the debrief is thorough. | Curry & Price, Introduction; Lasgah Pol | The journal is the record; the AAR document. |
| Comparable role level; a shared purpose. | Curry & Price | Scenario validation. |

### 8.2 The AAR document

At game end, the GameMaster produces `~/.harwness/matrix/<game-id>/aar.md` (plus `journal.jsonl`, a copy of `scenario.toml`) — see `harw-matrix-game/src/aar.rs`:

1. **Header**: scenario, purpose, seed, models, round count, a note "insight, not forecast."
2. **End state** of all tracks including hidden ones, a table of the trend per round.
3. **Timeline**: every argument with pros, cons, weights, modifier, target, roll, outcome, umpire summary.
4. **Disclosure**: all secret arguments (with commitment verification), all private channels, all `private_note`s — this is where the fog turns into a teaching moment.
5. **Goal attainment** per faction (public/secret, 0–3 with a rationale).
6. **Key moments & alternatives**: the umpire names 2–3 turning points and suggests fork rounds.
7. **Player debrief** (if `debrief_players`): each player agent receives full disclosure and answers: what did you want, what happened, what surprised you, what would you do differently?
8. **Plausibility check** (Sabin's validation questions) and **facilitator interventions** (all injects/overrides).

When the run is in business mode, the same run also produces a paper-ready `report.md` (§7.1, `harw-ops/src/matrix/report.rs`), covering purpose and key questions, actors, a round-by-round log, market shares, outcome, an after-action review structured around four questions (planned / happened / why / lessons), recommendations and open questions, and methodology (dice from the engine only, seed, replay). The UIA can hand `report.md` to `uia-latex-writer` (template `business-paper`) to render an optional `.tex`/`.pdf` version.

---

## 9. Testable invariants

### 9.1 Visibility (property tests, `proptest`)

Generator: random journals of events with random audiences, players, channels, secret arguments, disclosures.

1. **Non-interference**: for every player *p*: `project(J, p) == project(J ∪ E, p)` for any set `E` of events with audience `Pair(a,b)`, `p ∉ {a,b}`, as well as `Seat(q)`, `q ≠ p`, as well as `UmpireOnly`. So the projection — and thus the rendered prompt — is **byte-identical** regardless of other parties' conversations. This covers "neither content nor existence."
2. **Umpire mode `none`**: `project(J, Umpire)` is invariant under all `Pair` events.
3. **Umpire mode `cited`**: the umpire sees channel *c* exactly when an argument in the current adjudication cites `c` and the citer is a member of `c`.
4. **Monotonicity**: every event in `project(J, p)` satisfies `visible_to(aud, p)`; after `SecretRevealed`, the event becomes visible to everyone, before that to nobody but the owner and the umpire.
5. **Prompt scan**: for generated scenarios with unique marker strings in every private/secret piece of content, no marker appears in the prompt of an unauthorized seat (end-to-end, through the real prompt builder with mock children).
6. **World variables**: `project` contains a variable exactly when its visibility includes the seat; secret arguments don't change publicly visible variables before their disclosure.

### 9.2 Determinism & replay

7. **Replay equality**: journal (including journaled LLM responses) + scenario → replay without model calls yields the same `state_hash`es at every round end and the same dice.
8. **Order independence**: permuting the completion order of parallel child calls changes neither dice nor state (derived seeds; insertion into the journal in canonical seat order).
9. **Override isolation**: an override on argument *x* changes no other argument's dice.
10. **RNG distribution**: statistical test (10⁵ seeds) — frequencies per target value within ±1% of the §4.2 table; a natural 2 always fails.
11. **Fork**: forking from round N with the same responses ⇒ identical to the original up to N.

### 9.3 Sealed simultaneous actions

12. **Seal**: no prompt in the *Arguments* phase contains content from an argument of the same round (test: mock children record prompts; marker search). `ArgumentsRevealed` is journaled only once all four submissions (or `Forfeit`) arrive.
13. **Commitment**: for every disclosed secret argument, `sha256(canonical(content) ‖ salt) == commitment`; canonical serialization is stable (golden test).
14. **No revision**: after `ArgumentSealed`, a second submission from the same seat in the same round is rejected.
15. **Effects before the roll**: `DiceRolled` for argument *x* always follows the `Adjudicated` entry that carries `on_success`/`on_failure` for *x* in the journal; applied ops ⊆ the chosen branch (or override).

### 9.4 Bounds & rules

16. `target ∈ [3, 11]` for every weight combination; weights outside {0,1,2} or a modifier outside [−2,2] → validation error.
17. Tracks always stay in `[min, max]`; `|Δ| ≤ max_track_step` per argument.
18. Big projects: never more than +1 stage per argument, never > `stages`.
19. The per-seat secret limit is never exceeded.
20. Leak guard: an umpire text with a 5-gram from an undisclosed pair channel is never journaled as `Public` (mock umpire deliberately leaking).

---

## 10. Crate layout

`harw-matrix-game` is the deterministic core with no model calls of its own (`harw-matrix-game/src/lib.rs`):

```
harw-matrix-game/
  src/lib.rs             // crate overview, re-exports
  src/scenario.rs        // harwness.matrix-scenario/v1 (classic and business), parsing, validation
  src/state.rs            // seats, audiences, world variables, journal (JSONL), state, replay
  src/visibility.rs       // project(journal, seat), leak guards (5-gram scanner)
  src/commitments.rs      // SHA-256 commitments for sealed/secret arguments
  src/dice.rs             // derived ChaCha20 seeds, 2d6 pro/con math, d100 ladder, resolve_conflict
  src/phases.rs           // phase FSM, JSON contracts, effect validation, deterministic GameMaster steps
  src/business.rs         // business-mode rules (`when`/`effect`) and the logit market model
  src/aar.rs              // after-action review as markdown
  src/prompts.rs          // system, turn, and correction prompts, built only from &SeatView
  src/events.rs           // MatrixGameEvent for the TUI panel
  src/library.rs          // inject library with variance packages (seed-deterministic selection)
  src/precedents.rs       // precedent register and selection of relevant rulings
  src/lessons.rs          // design lessons in the AAR, AarSummary, compare_runs for multi-run comparison
  src/error.rs            // MatrixError / MatrixResult
  src/extension_tests.rs  // tests for the §11 extensions
```

Dependencies: `harw-core` (`ChildController`), `harw-types`, `serde`, `toml`, `sha2`, `rand_chacha`.

The orchestration and TUI-facing layer lives in `harw-ops/src/matrix/`, on top of this crate:

```
harw-ops/src/matrix/
  mod.rs          // /matrix (read-only: show, list, replay, compare), run bookkeeping
  game_master.rs  // matrix.draft_scenario / matrix.status / matrix.start / matrix.run / matrix.finish
  runner.rs       // drives seat agents as children via ChildController, per-seat turn timeout
  report.rs       // build_report(): paper-ready report.md (business mode)
```

`harw-tui` depends on `harw-matrix-game` only indirectly, through `harw-ops`' observer stream (`project(Observer)`) and the read-only `/matrix` command; it holds no matrix game state of its own.

Not implemented: teams (several agents per faction), more than 4 players, a human player occupying a seat directly (the architecture allows it: a seat could take TUI input instead of a child session), the S.C.R.U.D. combat resolution from Curry & Price, the simple-narrative system, and voting procedures. Maps/graphs in the TUI (currently tracks and lists only) are also open.

---

## 11. Extensions: behavior, Red Cell, suspicion, precedent, variance

All five building blocks are optional; a scenario without them behaves byte-identically to before (new state fields serialize to nothing when empty, so `state_hash` of older journals stays valid). Every effect goes through journal entries; prompts still come only from `SeatView`. Implemented in `harw-matrix-game/src/scenario.rs` (behavior profiles), `phases.rs` (Red Cell, suspicion effect op), `precedents.rs`, and `library.rs`.

### 11.1 Behavior profile per faction/team

`[factions.behavior]` (or `[teams.behavior]`) directly under the faction/team entry: `rules` (≤ 6 rules of action), `risk` (0 = risk-averse … 1 = risk-seeking; default 0.5), `loss_framing` (whether the situation is experienced as a threatened loss relative to a reference point), `anchor` (the reference point), `red_lines` (2–5, required once a profile exists). The profile is private: spelled out in that seat's system prompt, journaled as `BehaviorBriefing` with audience `Seat(p)`, and repeated as a short reminder (risk, framing, anchor, red lines) at the start of **every** turn prompt for that seat — even in a pure delta. The umpire, Red Cell, and other seats never see it; the AAR discloses it.

### 11.2 Red Cell

`[red_cell] enabled = true, sharpness = 0..1` (only with `argument_system = "pros_cons"`). A new seat, `Seat::RedCell` (agent role `matrix-redcell`), sees only public information, has no goals and no victory condition. In the counter-argument phase, after all players, one more call follows with the `red_cell_objection` contract: `{target, assumption, cons}` against the load-bearing assumption of the leading public argument — or explicitly `{"no_objection": true}`. Sharpness sets the tone and the maximum number of cons (1/2/3). `submit_red_cell` journals a public `RedCellObjection` and appends the cons under the reserved key `red_cell` on the target; the umpire weighs them in `con_weights["red_cell"]` like any other con (`red_cell` is a reserved seat ID and never appears in `standing`).

### 11.3 Suspicion ladder for secret arguments

Stages per secret: unnoticed → rumor → suspicion → evidence → exposed. Effect op `{"op": "raise_suspicion", "secret_id", "by": 1|2}` — only in rulings on public arguments or in injects, not in `each_round`, at most two stages per effect branch and secret. Publicly only a fixed Rust template (`SuspicionRaised`) appears, containing nothing beyond the already-announced secret ID. At the top stage, regular disclosure follows, with the salt (`RevealedBy::Suspicion`). The level is shown in every seat's situation report and in the umpire's adjudication task.

### 11.4 Precedent register

The umpire marks a ruling on a **public** argument with `precedent: {principle, tags}` (1–5 keywords; a contract error on secret arguments). Rust journals it publicly as `PrecedentSet` (`p1`, `p2`, …, with net and probability). Later adjudication prompts cite up to five relevant standards from earlier rounds — chosen deterministically by keyword match and long-word overlap with the round's arguments, taken only from the umpire's own projection. `principle` is public text and goes through the leak guard like `public_rationale`. The AAR keeps a precedent register, including later cases a standard would have applied to.

### 11.5 Inject library, multi-run comparison, design lessons

`[[inject_packages]]` with `id`, `label`, `max_injects` (1–3) and candidates `[[inject_packages.injects]]` (`id`, `text`, `audience`, `effects`, optional `earliest`/`latest`, `attributed`). A run picks one package (given explicitly or by seed) and draws at most three injects from it, never two in the same round and never in a round that already has a fixed scenario inject. Selection depends only on the master seed and the scenario, is recorded as `InjectPackageSelected` (observer only) in the journal, and is recomputed on replay; `package_injects_for_round` supplies the injects due each round.

The AAR carries a **Design Lessons** section: timing of secret arguments (and whether they surfaced only at game end), clustering of target values/ladder rungs, plausibility of the dice (early rolls: mean pip sum and successes against expectation as a z-score), and resulting notes for scenario design. For multi-run comparisons across seeds, `summarize_run` condenses each run into an `AarSummary`; the pure function `compare_runs(&[AarSummary])` lines up metrics and end values side by side and separates robust from sensitive quantities. In the TUI, `/matrix compare <run> <run> …` compares runs from the current session and writes the report as `compare-<runs>.md` next to the run directories; `matrix.start` on the orchestrator side can pick an inject package explicitly (without one, the seed chooses).
