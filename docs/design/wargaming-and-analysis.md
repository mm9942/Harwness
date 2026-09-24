# Business Wargaming, Analytical Tradecraft, and Visualization

> Status: partially implemented · Last reviewed: 2026-09-24

This document extends `docs/design/matrix-game.md` (the matrix-game core: GameMaster, player agents, scenario schema `harwness.matrix-scenario/v1`). Where it assumes a type or field name from the core, `matrix-game.md` wins on any mismatch; the extensions introduced here are additive.

Sources (study material, paraphrased and summarized in our own words):

- Oriesek & Schwarz, *Business Wargaming* (teams, moves, gamebook, debriefing, telecom/automotive case studies, early-warning systems).
- Hall & Citrenbaum, *Intelligence Analysis: How to Think in Complex Environments* (decomposing hypotheses into observables, critical thinking, mirror imaging, assumption checking, red teaming).
- Herman, *Intelligence Power in Peace and War* (single-source vs. all-source, the refining funnel, the intelligence cycle and its limits, need-to-know).
- Zelazny, *Say It With Charts* (message → comparison type → chart form).
- Not from the study material, added for completeness: Analysis of Competing Hypotheses (Heuer), the Admiralty/NATO source-rating scale (A–F/1–6). Both are standard practice and are treated here as such.

---

## 0. Summary

1. **Business mode** (`mode = "business"`) turns the matrix game into an Oriesek-style strategy wargame: company, competitor, and market teams as player agents, the Rust `GameMaster` as control/white cell, structured decision templates, a pragmatic market model that a bound adjudicator can override, shocks as injects, and a structured debrief (observation → lesson → implication → recommendation). This is implemented in `harw-matrix-game` (see §1 for what's built vs. still open).
2. **Analytical tradecraft** is planned as additive fields on `ResearchFinding` (hypotheses, assumptions, indicators, source grading, likelihood language kept separate from confidence) plus a set of skills that would teach agents to use them. The schema fields are implemented in `harw-research`; the skills themselves are not yet built (§2).
3. **Intelligence cycle**: orchestrator = tasking/all-source analysis, worker = single-source collection + first-pass processing. Need-to-know is already a core principle (`ContextCeiling::intersect`, per-child context scoping) and is carried over to matrix-game visibility. This mapping is conceptual; no dedicated `all-source-analyst`/`red-teamer` agents exist yet (§3).
4. **Visualization**: every chart should have a message-first title; the comparison type should pick the ratatui widget. Not yet implemented (§4).
5. Status of each piece, tracked in §5.

---

## 1. Business-wargame mode for the matrix game

> Status: implemented (`harw-matrix-game/src/scenario.rs`, `business.rs`; orchestration in `harw-ops/src/matrix/`). Some KPI and stakeholder detail described below is not modeled yet — see the notes per subsection.

### 1.1 What sets a business wargame apart from a training simulation

Oriesek/Schwarz draw a sharp line between a business wargame and a business-school simulation: in the latter, the "right" levers live in the model and are figured out after a few rounds. A wargame, by contrast, is designed for **one specific organization and specific key questions**; the insight comes from the teams' interaction, and the computational model is only a tool for the control team, which may correct it at any time. For us that means:

- The market model in the `GameMaster` is **deterministic, small, and overridable** — it produces a proposal, not a ground truth. Every override is logged with a rationale (traceability was a success factor in the book: without hard data, you must at least be able to explain afterward why something turned out the way it did).
- Every scenario starts with **key questions** (`key_questions`) that the debrief must explicitly answer at the end. Without them the game cannot start (a validation error).

### 1.2 Role model → our topology

The book names four basic elements: the company team, competitor teams, the market team, and the control team. Control also plays every stakeholder not explicitly cast (regulator, smaller competitors, an acquisition target, interest groups) and can inject shocks.

| Book role | Harwness | Implementation |
|---|---|---|
| Company team ("Blue") | player agent `company` | plays the current strategy plan (or an alternative, `strategy_variant`) |
| Competitor 1..n ("Red") | player agents `competitor_a`, `competitor_b` | get a gamebook profile; directive "be the enemy" |
| Market team | player agent `market` **or** a white-cell sub-agent of the GameMaster | assigns relative attractiveness/market shares |
| Control / white cell | the Rust `GameMaster` (+ optional LLM adjudicator) | schedule, rules, KPI computation, shocks, remaining stakeholders, disclosure |
| Regulator, minor competitors, acquisition targets | played by the `GameMaster` (`[[business.stakeholders]]`) | decisions via rule or adjudicator prompt |
| Coach / devil's advocate per team | not a separate skill yet — see §2 | intended to challenge assumptions, enforce template discipline |

With **four player slots**, there are two workable layouts (scenario field `business.market_role`):

- `market_role = "player"` (default): company, competitor_a, competitor_b, market. The market is then an evaluating player — it doesn't submit offers, only assessments (its own `market_assessment` template).
- `market_role = "white_cell"`: the market runs as an internal sub-agent of the GameMaster; the fourth slot becomes a third competitor. The book's automotive case study shows a proven trio: the actual main competitor, a newcomer from an adjacent market, and a fictional aggressive low-cost entrant.

The market team is an evaluator, not a competitor; it may not use private channels to seller teams except for explicitly modeled customer conversations (channel type `sales_call`, read by the GameMaster).

### 1.3 Move structure

The book typically uses three moves; the first starts in the present on real data, later moves jump forward in time (e.g. +1 year, +2 years, +4 or more years, the last one as "foresight"). A move is a decision cycle. The telecom case study structures it as: market entry — positioning against competitors — long-term consolidation.

Mapping onto GameMaster phases per move (`MovePhase` in the game-master state machine):

```
Brief ─▶ Deliberation ─▶ Submission ─▶ Plenary ─▶ MarketAssessment ─▶ Adjudication ─▶ Hotwash ─▶ (next move)
  │          │ private pair channels (GM cc'd)   │ public pitches       │ market rating    │ KPI computation, shocks
  │          │ decision template                  │ (all see everything) │                  │ disclosure
  └ Move brief: public situation report + private team report (KPIs, P&L excerpt)
```

- **Brief**: the GameMaster distributes `public_situation` (all) and `team_report` (team only) — the move starts from the previous move's outcome. A team that "bought" market share now sees an emptier war chest and constrained options (budget from `cash`).
- **Deliberation**: internal team discussion (one agent per team; optionally child agents as team roles: lead, briefer, communicator — the telecom case study names exactly these three plus a coach). Contact with other teams **only** through private pair channels; the GameMaster silently reads every channel (in the book: every email between teams is automatically copied to control).
- **Submission**: a structured decision template (§1.5). Deadlines are hard; a late team gets the prior decision as "status quo" (the book stresses that a lagging team throws the whole game off pace).
- **Plenary**: every seller team delivers a public pitch (value proposition, max N tokens). From here everyone has the same information about the offers on the table.
- **MarketAssessment**: the market team rates attractiveness per segment (0–10) with a rationale; the computational model produces a share proposal.
- **Adjudication**: the GameMaster consolidates hard data (price, investment) and soft data (market ratings) into KPIs, checks deals against rules (antitrust, financial capacity), decides stakeholder reactions, and plays due injects.
- **Hotwash**: a short mid-game lesson after every move (the book does this after each move too). Every player supplies three sentences — "what worked / what didn't / what surprised me" — which the GameMaster appends to the AAR record.

**"Attack-counter" variant** (`business.variant = "attack_counter"`, after the book's automotive case study): move 1 has competitors attack (with fictionally unlimited but legally and plausibly bounded capital), move 2 has the company develop counter-strategies, move 3 has the attackers react to an "intercepted" counter-strategy. An "ambassador" carries a group's intent into the next move — for us, a GameMaster-generated summary that goes into the counterpart's context as `ambassador_brief`. This variant needs no market team and suits blind-spot analysis well.

### 1.4 Injects (market and scenario shocks)

The book describes shocks as control interventions that force teams to address a topic (a product recall, a safety concern, a regulatory change). We distinguish:

| Kind | Trigger | Visibility | Example |
|---|---|---|---|
| `scheduled` | fixed move/phase | public or a team list | consumer-market opening from move 2 |
| `conditional` | predicate over world state | public | a team's share > 40% → antitrust review |
| `random` | seed-deterministic roll with `p` | private | a supplier failure at `competitor_b` |
| `control_discretion` | adjudicator proposal, confirmed by the GameMaster | variable | a press leak of an alliance talk |

All injects are declared in the scenario; the adjudicator may only choose **from the catalog** (the LLM cannot invent new world facts). Randomness is seed-based so a game can be replayed reproducibly — the book reports that repeated plays with different participants unfold differently but yield similar lessons, a robustness we want to be able to measure with `matrix compare` (§11.5 of `matrix-game.md`).

### 1.5 Strategic decision template

In the book, templates are the medium through which teams give the market and control hard data. For us the template is a tool call, `submit_move`, with a JSON schema (generated by the GameMaster from the scenario):

```json
{
  "team": "competitor_a",
  "move": 2,
  "strategic_intent": "Price leadership in the SME segment, hold premium positioning elsewhere",
  "actions": [
    {"kind": "price",    "segment": "sme",     "offer": "cloud_basic", "value": 19.0},
    {"kind": "invest",   "area": "sales",      "amount_meur": 12},
    {"kind": "product",  "offer": "cloud_ai",  "change": "launch", "segments": ["enterprise"]},
    {"kind": "alliance", "with": "company",    "proposal_ref": "ch-a-c/msg-14"},
    {"kind": "lobby",    "target": "regulator", "topic": "data_residency"}
  ],
  "assumptions": [
    {"id": "A1", "text": "company will not cut prices before move 3", "confidence": "medium"}
  ],
  "expected_reactions": {"company": "bundled offer", "competitor_b": "niche retreat"},
  "expected_kpis": {"share_sme": 0.30, "ebit_meur": 4.0},
  "pitch": "…public text for the plenary…"
}
```

Key rules:

- `actions[].kind` is a closed set from `business.action_kinds`; unknown action kinds fall through to plain matrix-game adjudication (argument + reasons) as `free_argument`.
- `assumptions` and `expected_reactions` are required. They are the raw material for the debrief: after the move, the GameMaster compares expectation against outcome ("forecast error" per team).
- Budget check: the sum of `invest` ≤ available funds; otherwise the submission is rejected with an error (one correction attempt, then status quo).

### 1.6 Adjudication in the business context

Three layers, in this order:

1. **Rule check (Rust, deterministic)**: budget, capacity, regulation (`business.rules`), antitrust thresholds. Deals between teams need matching `alliance`/`acquisition` actions from both sides with the same `proposal_ref`. — Implemented via `business::evaluate_predicate` and `restriction_for`; a matched `restrict_actions` rule becomes a veto in adjudication (`phases::resolve_argument`), and `require_stakeholder` becomes an umpire note.
2. **Market model (Rust, deterministic)**: an attractiveness model per segment — `share_i ∝ exp(β · score_i)`, where `score` is a weighted sum of the market rating (soft), price position, sales investment (with diminishing returns), and product fit. Implemented as `business::apply_market_model` (`logit_share`), applied at round end via `phases::close_round_with_market`, and journaled as a `WorldDelta`. Output today is limited to **market share**; revenue, contribution margin, EBIT, and cash as further KPIs are open — the book recommends pragmatic models that stay scoped to the key questions, which is also why we started with the smallest useful KPI.
3. **Control judgment (LLM adjudicator, optional, bounded)**: may correct model results within `business.override_bounds` (e.g. ±5 percentage points of share) and must write a rationale for doing so. The book describes exactly this case: a team chose an extreme low-cost strategy that the cost model would have unfairly punished, so control manually adjusted costs. Actions outside the catalog (`free_argument`) are judged by plain matrix-game logic: argument with reasons, counter-arguments from affected teams, then a probability (`almost_certain`/`likely`/`even`/`unlikely`/`remote`) and a seed-deterministic roll. — **Open**: the bounded-override mechanism (`AdjudicationOverride { field, from, to, rationale }`) as a distinct, journaled event type is not yet implemented; today an override would have to go through the general effect-validation and facilitator-note path.

Stakeholder decisions (a regulator approving an acquisition only with conditions, a target board declining) are made by the GameMaster via rule or adjudicator, using the stakeholder profile from the scenario.

### 1.7 Information parity and disclosure

In the book, control actively spreads certain information (e.g. a concluded alliance) because it would become public through the media in reality — this preserves information parity. We plan to implement this as declarative disclosure rules:

```toml
[[business.disclosure]]
on = "alliance_signed"      # event in the GameMaster
audience = "all"            # or a team list
delay_phases = 0
template = "{a} and {b} announce a partnership: {summary}"
```

> Status: open. Declarative `[[business.disclosure]]` rules are not implemented; today, disclosure of business-mode events follows the general public/private mechanics of §2 in `matrix-game.md` (an umpire or the GameMaster narrating a fact publicly), without a dedicated templated-disclosure table.

Private channel content is **never** disclosed verbatim, only as a message the GameMaster formulates (see compartmentation, §3.3).

### 1.8 Debriefing and insight capture

The book treats the detailed debrief as the most important part; it draws on coach observations, participant input, model data, and analysis of the full message traffic (who talked to whom about what, patterns like "everyone sought similar alliances in the same move"). The structure is deductive: observations → lessons → implications → concrete recommendations back into the strategy plan.

`harw-matrix-game/src/aar.rs` implements the AAR for both classic and business-mode runs (§8.2 of `matrix-game.md`), and `harw-ops/src/matrix/report.rs` additionally builds a paper-ready `report.md` for business-mode runs, including market shares start→end per team and segment, and an after-action review structured around four questions (planned / happened / why / lessons) — this fulfills the deductive structure described above. Channel-traffic pattern analysis (a team×team contact matrix, first mention of deal keywords), a fixed bias checklist (from the book's automotive case study), and a `CounterApproach` taxonomy (ignore / prevent / provide / prepare / convert / reduce / anticipate) as a distinct, structured AAR field are **open** — not modeled as their own data structures yet; a human or the umpire can still capture this qualitatively in the narrative AAR text today.

### 1.9 A complete example scenario

```toml
schema = "harwness.matrix-scenario/v1"
id = "cloud-sme-2027"
title = "SME cloud: a hyperscaler's market entry"
mode = "business"
seed = 20270101
language = "en"

[game]
moves = 3
# simulated time per move (move 3 = foresight)
move_labels = ["2027 – market entry", "2028 – positioning", "2031 – consolidation"]
phase_deadline_turns = 6          # max agent turns per phase and team
max_channel_messages_per_move = 12

[[game.key_questions]]
id = "KQ1"
text = "Does our premium positioning hold if a hyperscaler undercuts SME prices by 30%?"
[[game.key_questions]]
id = "KQ2"
text = "Is an alliance with a regional systems-integrator consortium worth more than expanding our own sales force?"
[[game.key_questions]]
id = "KQ3"
text = "Which regulation (data residency) changes the rules of the game, and who benefits?"

[business]
variant = "strategy_test"         # | "attack_counter" | "crisis_response"
market_role = "player"            # | "white_cell"
currency = "MEUR"
override_bounds = { share_pp = 5.0, cost_pct = 15.0 }
action_kinds = ["price", "invest", "product", "alliance", "acquisition", "lobby", "campaign", "free_argument"]

[business.market_model]
kind = "logit_share"
beta = 0.9
weights = { market_score = 0.45, price_position = 0.30, sales_invest = 0.15, product_fit = 0.10 }
sales_invest_saturation_meur = 20
kpis = ["share"]

[[business.segments]]
id = "sme"
name = "SME (10-249 employees)"
size_meur = [800, 950, 1300]      # per move
price_sensitivity = 0.8
[[business.segments]]
id = "enterprise"
name = "Enterprise"
size_meur = [1200, 1260, 1400]
price_sensitivity = 0.35

[[business.rules]]
id = "antitrust"
when = "share(segment) > 0.40 && action.kind == 'acquisition'"
effect = "require_stakeholder:regulator"
[[business.rules]]
id = "cash_floor"
when = "cash < 0"
effect = "restrict_actions:[invest,acquisition]"

[[teams]]
id = "company"
role = "company"
display = "NordCloud AG (us)"
agent = "matrix-player"
strategy_brief = "gamebook/nordcloud.md"
start = { cash = 60, share = { sme = 0.34, enterprise = 0.22 }, capacity = 1.0 }

[[teams]]
id = "competitor_a"
role = "competitor"
display = "Hyperscaler X (SME newcomer)"
agent = "matrix-player"
strategy_brief = "gamebook/hyperscaler-x.md"
directive = "Be the enemy: win SME share, legally, plausibly, aggressively."
start = { cash = 500, share = { sme = 0.05, enterprise = 0.30 }, capacity = 3.0 }

[[teams]]
id = "competitor_b"
role = "competitor"
display = "ValueHost GmbH (fictional low-cost provider)"
agent = "matrix-player"
strategy_brief = "gamebook/valuehost.md"
start = { cash = 25, share = { sme = 0.28, enterprise = 0.03 }, capacity = 0.8 }

[[teams]]
id = "market"
role = "market"
display = "Market panel (SME and enterprise)"
agent = "matrix-market"
strategy_brief = "gamebook/market-panel.md"
assessment_scale = [0, 10]

[[business.stakeholders]]
id = "regulator"
display = "Data-protection/competition authority"
played_by = "gamemaster"
profile = "Reviews acquisitions above 40% segment share; sensitive to data residency."
[[business.stakeholders]]
id = "sysint_alliance"
display = "Regional systems-integrator consortium"
played_by = "gamemaster"
profile = "Seeking an exclusive platform partner; wants 20% margin and lead protection."

[channels]
pairwise = "all_players"          # private pair channels between all teams
observer = "gamemaster"           # the GameMaster reads every channel (cc'd to control)
market_contact = "sales_call_only"

[[injects]]
id = "INJ-1"
kind = "scheduled"
at = { move = 2, phase = "brief" }
audience = "all"
text = "Draft bill: personal SME data must be processed domestically starting 2029."
[[injects]]
id = "INJ-2"
kind = "conditional"
when = "share('competitor_a','sme') > 0.25"
audience = "all"
text = "Trade press: antitrust authority reviewing SME cloud price-cutting."
[[injects]]
id = "INJ-3"
kind = "random"
p = 0.25
at = { move = 3, phase = "brief" }
audience = ["competitor_b"]
text = "Your data-center operator cancels the contract; 20% of capacity is out for one move."
[[injects]]
id = "INJ-4"
kind = "control_discretion"
audience = "all"
text = "An alliance conversation is publicly leaked."
```

Gamebooks (`gamebook/*.md`) are part of the scenario package: a one-page profile per team (strategy, finances, strengths/weaknesses), plus a shared market overview. The book emphasizes that all participants get the same baseline; team-private additional information belongs in `strategy_brief` and is visible only to that team.

Validation on load: `key_questions` ≥ 1; exactly one `company`; `market_role = "player"` ⇒ exactly one team with `role = "market"`; starting shares per segment sum to ≤ 1; all `injects.when`/`rules.when` are parseable; `size_meur` length matches `moves`. See `harw-matrix-game/src/scenario.rs::validate_business` for the implemented checks.

---

## 2. Analytical tradecraft for analyst/researcher/explorer agents

> Status: partially implemented. The `ResearchFinding` schema extensions below are implemented in `harw-research`. The skills and agent assets that would teach agents to use them are **open** — not yet created.

### 2.1 Choice of techniques

| Technique | Core idea (our summary) | Form | Why |
|---|---|---|---|
| Hypothesis decomposition | Hall/Citrenbaum: break a hypothesis into expected activities, transactions, behavior ("if this is true, I should see X") and use that to steer collection | skill `hypothesis-decomposition` (open) | makes explorer assignments checkable |
| Analysis of Competing Hypotheses | test several hypotheses against all evidence in a matrix; the least-refuted one wins; diagnosticity over confirmation | skill `analysis-ach` (open) + finding field `hypotheses` (implemented) | counteracts confirmation bias |
| Key Assumptions Check | make assumptions explicit, test them like hypotheses, decompose into observables (Hall: weak assumptions are often the single point of failure) | skill `key-assumptions` (open) + field `key_assumptions` (implemented) | assumptions otherwise disappear into prose |
| Red team / devil's advocate | argue the counterparty's position, specifically against mirror imaging | skill `red-team` (open); in the matrix game, a coach role | Hall repeatedly names red teams as a countermeasure |
| Indicators & warnings | observable signals per scenario/assumption with threshold and check frequency; early warning against surprise | skill `indicators-warnings` (open) + field `indicators` (implemented) | connects a wargame AAR to ongoing monitoring |
| Source grading | source reliability (A–F) kept separate from information credibility (1–6); check independence | skill `source-grading` (open) + fields on `SourceReference` (implemented) | Herman: collectors must vet their own sources for unreliability/deception |
| Likelihood language | Herman: analysis's job is to convey the range of uncertainty as precisely as possible, usually via coded terms | field `likelihood` next to `confidence` (implemented) | separates "how likely" from "how solid" |

Not adopted (for now): Hall's cultural/semiotic analysis — no use for code/dependency research; anomaly and trend analysis fold implicitly into `indicators-warnings` (baseline + deviation).

### 2.2 Skill manifests (open — not yet created)

The skills below are designed against the format in `harw-config/src/skill_toml.rs` (`name`, `enabled`, `description`, `instructions_file` — default `instructions.md`, `tools`, `mcps`; `deny_unknown_fields`). All would be **read-only**: they change nothing, only structure thinking and the returned finding. None of `harw-home/assets/skills/{analysis-ach,key-assumptions,red-team,indicators-warnings,source-grading,hypothesis-decomposition}/` exist yet; the outlines below record the intended design.

#### `analysis-ach`

```toml
name = "analysis-ach"
description = "Analysis of Competing Hypotheses: test several explanations against all evidence, diagnosticity over confirmation."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

Intended structure: (1) **When**: the question has ≥ 2 plausible explanations (cause of a bug, a competitor's intent, reason for a regression). (2) **Form hypotheses**: at least 3, mutually exclusive, including one "uncomfortable" one and a null hypothesis; one sentence each, `H1..Hn`. (3) **Evidence list**: every piece of evidence once, with a source rating (see `source-grading`); the absence of expected evidence counts too. (4) **Matrix**: each evidence item × hypothesis, `consistent | inconsistent | neutral`; evidence consistent with everything is marked non-diagnostic. (5) **Scoring**: rank hypotheses by the number of weighted inconsistencies, never by the number of confirmations. (6) **Sensitivity**: which 1–2 pieces of evidence are decisive? What if they're wrong or manipulated? (7) **Return**: `hypotheses[]` with `status` and `inconsistency_score`; the conclusion names the leading hypothesis and the next collection task that best separates H1 from H2. (8) **Anti-patterns**: only one hypothesis; picking evidence that fits; `high` confidence with two neck-and-neck hypotheses.

#### `key-assumptions`

```toml
name = "key-assumptions"
description = "Surface load-bearing assumptions, justify them, test them like hypotheses, and decompose them into observable checks."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

Intended structure: (1) note the conclusion; (2) phrase every unstated precondition as a sentence (trigger words: "of course," "surely," "as always"); (3) per assumption: *why do we believe this? Under what circumstances would it be false? Has it been false before?*; (4) classify as `supported | caveated | unsupported`; (5) a mirror-imaging check per Hall — warning phrases like "they'd never do that, it makes no sense"; (6) every `unsupported` assumption gets ≥ 1 indicator; (7) return `key_assumptions[]`; rule: one `unsupported` assumption on the critical path caps `confidence` at `medium`.

#### `red-team`

```toml
name = "red-team"
description = "Take the counterparty's position, surface mirror imaging and groupthink, formulate the strongest alternative."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

Intended structure: (1) adopt the role: goals, constraints, information state, and culture of the counterparty (competitor, attacker, reviewer, user) — not one's own; (2) "how would I beat/break this plan?" — three concrete, legal, plausible attacks; (3) the strongest counter-thesis in 5 sentences (devil's advocate, even if not believed); (4) a groupthink check: which position did nobody on the team hold?; (5) Hall: criticism without a proposal isn't enough — a non-self-serving countermeasure per objection; (6) return as `dissent[]` + `alternative_hypotheses`; (7) usage: as a child agent with a **narrower** context ceiling (sees the conclusion and evidence, not the author's deliberation — avoids anchoring).

#### `indicators-warnings`

```toml
name = "indicators-warnings"
description = "Translate scenarios and assumptions into observable indicators with a baseline, threshold, source, and check cadence."
instructions_file = "instructions.md"
tools = ["filesystem.read", "web.search", "web.fetch"]
```

Intended structure: (1) input: a scenario, hypothesis, assumption, or AAR recommendation; (2) decompose per Hall: "if this happens, I'd first see …" — activities, transactions, technical traces, **and** the absence of normal activity; (3) per indicator: baseline (normal), threshold (warning), source/collector, check cadence, lead time; (4) diagnosticity: does the indicator distinguish between scenarios, or fire for all of them?; (5) weak signals (Oriesek/Schwarz's early-warning chapter): targeted scanning in the search fields the wargame identified; (6) return `indicators[]`; optionally filed as a follow-up card.

#### `source-grading`

```toml
name = "source-grading"
description = "Rate evidence by source reliability (A-F) and information credibility (1-6); spot dependencies between sources."
instructions_file = "instructions.md"
tools = ["filesystem.read", "web.fetch"]
```

Intended structure: (1) keep two axes strictly separate: *who* says it (track record, closeness to the matter, interests) vs. *what* is said (confirmed by something independent? plausible? consistent?); (2) mapping for our source classes: `Standard`/`OfficialDocs`/`CargoRegistrySource` → typically A–B, `ReleaseNotes`/`Repository` → B–C, `Web` → C–F; local source code is a primary source (A) for "what the code does," not for "what it's supposed to do"; (3) spot circular confirmation: two blog posts citing the same press release are *one* source (`derived_from`); (4) check for deception/staleness (date, version, digest); (5) return: per `SourceReference`, `reliability`, `credibility`, `derived_from`.

#### `hypothesis-decomposition`

```toml
name = "hypothesis-decomposition"
description = "Decompose a hypothesis or question into concrete, checkable observations and derive bounded collection tasks from them."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

Intended structure: (1) name the driver — Hall distinguishes hypothesis, expectation, hunch, and puzzle; all decompose the same way; (2) the functions that must hold if the hypothesis is true; (3) per function, what's observable (files, log lines, versions, metrics); (4) turn those into `ResearchQuestion`s with `scope`, `expected_output`, `stop_condition` for explorer children; (5) feedback: which observation would *refute* the hypothesis?

#### Agent assignment (open)

Intended: `harw-home/assets/agents/source-researcher/agent.toml` gains `skills = ["dependency-research", "source-grading"]`; a new worker `all-source-analyst` gets `skills = ["analysis-ach", "key-assumptions", "source-grading", "indicators-warnings"]`; a new worker `red-teamer` gets `skills = ["red-team", "key-assumptions"]` with a small context budget; the matrix player agent (`matrix-player`) gets `skills = ["red-team"]` in business mode (coach function), the market player gets none. None of these agent assets exist yet.

### 2.3 Changes to the research schema (`harw-research`)

> Status: implemented. All fields are additive via `#[serde(default)]`, so older findings still deserialize.

```rust
// harw-research/src/types.rs

/// Source reliability (Admiralty/NATO axis 1).
#[serde(rename_all = "snake_case")]
pub enum SourceReliability { A, B, C, D, E, F }   // F = cannot be judged

/// Credibility of the information (axis 2).
pub enum InfoCredibility { Confirmed, ProbablyTrue, PossiblyTrue, Doubtful, Improbable, CannotJudge }

pub struct SourceReference {
    // … existing fields …
    #[serde(default)] pub reliability: Option<SourceReliability>,
    #[serde(default)] pub credibility: Option<InfoCredibility>,
    /// Locator of another source this one depends on (circular confirmation).
    #[serde(default)] pub derived_from: Option<String>,
}

/// Estimated likelihood of the statement — kept separate from `Confidence`
/// (how solid the analysis is).
pub enum Likelihood { AlmostCertain, VeryLikely, Likely, RoughlyEven, Unlikely, VeryUnlikely, Remote }

pub enum HypothesisStatus { Leading, Viable, Weakened, Refuted }
pub struct HypothesisAssessment {
    pub id: String,                  // "H1"
    pub statement: String,
    pub status: HypothesisStatus,
    #[serde(default)] pub consistent_evidence: Vec<usize>,    // indices into `evidence`
    #[serde(default)] pub inconsistent_evidence: Vec<usize>,
    #[serde(default)] pub inconsistency_score: f32,
}

pub enum AssumptionStatus { Supported, Caveated, Unsupported }
pub struct KeyAssumption {
    pub id: String,                  // "A1"
    pub statement: String,
    pub status: AssumptionStatus,
    #[serde(default)] pub on_critical_path: bool,
    #[serde(default)] pub rationale: String,
}

pub struct Indicator {
    pub id: String,                  // "I1"
    pub observable: String,
    pub supports: Vec<String>,       // hypothesis/assumption IDs
    #[serde(default)] pub baseline: String,
    #[serde(default)] pub threshold: String,
    #[serde(default)] pub check_every: Option<String>,   // "7d", "per-release"
}

pub struct ResearchFinding {
    // … existing fields …
    #[serde(default)] pub likelihood: Option<Likelihood>,
    #[serde(default)] pub confidence_rationale: String,
    #[serde(default)] pub hypotheses: Vec<HypothesisAssessment>,
    #[serde(default)] pub key_assumptions: Vec<KeyAssumption>,
    #[serde(default)] pub indicators: Vec<Indicator>,
    /// Dissenting assessments (red team, a second analyst).
    #[serde(default)] pub dissent: Vec<String>,
}
```

Validation rules in `harw-research/src/validate.rs::validate_finding` (in addition to the existing evidence requirement from `Medium` up):

1. `hypotheses` non-empty ⇒ at least 2 entries and exactly one `Leading`; evidence indices must exist in `evidence`.
2. `confidence >= High` ⇒ at least one source with `reliability ∈ {A, B}` **or** two sources that are not related via `derived_from`.
3. A `KeyAssumption { status: Unsupported, on_critical_path: true }` ⇒ `confidence <= Medium`.
4. `confidence == Verified` ⇒ at least one source with `credibility == Confirmed`.
5. `confidence >= Medium` ⇒ `confidence_rationale` non-empty.

`FindingBundle` carries `#[serde(default)] pub requirements_trace: Vec<(QuestionId, Vec<usize>)>` — which findings answer which question; `coverage_gaps` remains. The JSON schema export includes the new fields so child agents see them in their prompts.

---

## 3. The intelligence cycle in the harness

> Status: conceptual mapping onto existing mechanisms; no dedicated new agent assets. Open items are called out per subsection.

### 3.1 Mapping the phases

Herman describes the process as a chain from collection (single-source reports) to all-source analysis that produces "finished" intelligence, then distribution to users; volume shrinks sharply along the way while the value per unit rises (his image: refining crude oil). He also warns that the clean cycle is an idealization — in practice, producers actively push results to users who tend to react rather than order, and first-pass processing directly steers what gets collected next.

| Phase | Harwness role | Artifact | Existing code |
|---|---|---|---|
| Tasking / requirements | root/child orchestrator | `ResearchQuestion` (question, scope, stop condition); a wargame's key questions = `key_questions` | `harw-research/src/types.rs`, `harw-plan` |
| Collection | explorer/source-researcher worker | raw evidence: `SourceReference` with a digest | `harw-explorer`, `harw-tool-web`, `harw-tool-fs`, `harw-lens` |
| Processing | the same worker (Herman: processing belongs close to collection) | normalized `ResearchFinding`, source rating | `validate.rs::parse_and_validate` |
| Analysis (all-source) | an `all-source-analyst` or the orchestrator | a fused finding with ACH, assumptions, indicators | open: no dedicated agent asset yet |
| Dissemination | return to the parent, TUI, knowledge base | `ReturnEnvelope`, AAR, knowledge artifact | `return_envelope.rs`, `harw-knowledge` |
| Feedback / re-tasking | orchestrator | new `ResearchQuestion`s from `unresolved_questions`, `coverage_gaps`, indicators | `FindingBundle.coverage_gaps` |

Consequences:

- **Separating single-source from all-source**: workers should draw conclusions only about their own source ("`Cargo.lock` says X"); the cross-source judgment is the analyst's job. This is a prompting convention today, not yet a schema-enforced one.
- **A funnel, not a pass-through**: children return compact findings, not raw data; raw data stays retrievable via a `FragmentReference` (digest). This is also token economy (`docs/design/token-efficiency.md`).
- **Taking the push model seriously**: workers may report `suggested_next_actions` and unexpected side findings (an optional `serendipity: Vec<String>` field on `ReturnEnvelope`) instead of only answering the assigned question. — Open: `serendipity` is not implemented on `ReturnEnvelope` yet.
- **Processing steers collection**: `hypothesis-decomposition` would generate follow-up questions directly; the orchestrator only decides budget/admission. — Depends on the not-yet-built skill.

### 3.2 Need-to-know as a principle

Herman defines need-to-know, in essence, as giving information only to those who must have it — not to everyone it might be useful to. He also warns against excessive secrecy, which hampers use and turns into turf behavior. We take both lessons:

**Child-agent context** (already exists in the core; stated here as a guideline):
- `ContextCeiling::intersect` can only narrow (`harw-context/src/ceiling.rs`) — need-to-know is thus structurally enforced.
- Convention (not yet a hard schema requirement): every delegation should carry a `need_to_know` note — which sections the child actually needs; the default is the question, its scope, and the minimum necessary fragments, not the parent's full history.
- Against over-compartmentalization: **tearline summaries** — a parent may hand a child a redacted short version plus a `FragmentReference` instead of the full fragment; the child can only pull the full text via an audited request, if the ceiling allows it. — Open: tearline summaries as a distinct mechanism are not implemented.
- Red-team children deliberately get *less* context (not the author's deliberation), so they can judge independently.

### 3.3 Compartmentation in the matrix game

| Information class | Visible to | Mechanism |
|---|---|---|
| Shared gamebook, `public_situation`, plenary, disclosed events | everyone | public channel |
| `strategy_brief`, `team_report`, own template | own team | team scope |
| Pair channel A↔B | A, B, the GameMaster (reading along) | private channel, `observer = gamemaster` |
| Model internals, seed, inject catalog, adjudicator rationale | GameMaster only | GameMaster scope; optionally in the AAR after the game ends |
| Market ratings (raw values) | market team, GameMaster | aggregated to public shares |

Rules:
1. **No information leakage through summarization**: when the GameMaster generates a situation report for team C, the underlying LLM call receives only inputs C is allowed to see (a context ceiling per recipient, the same mechanism as §3.2). This is the most important technical invariant; a canary-word test checks that a word planted in channel A↔B never appears in any report to C. This follows directly from the projection mechanism in `matrix-game.md` §2.
2. **Source-protection rule** (following the practice Herman describes, of acting on protected sources only with independent cover): a team may not cite in a public pitch information it only knows from a private channel without giving up the source; the GameMaster flags violations in the AAR (non-blocking). — Open: not implemented as an automated check yet; today this is a norm stated in the player prompt, not enforced by code.
3. **Deception is allowed** in pair channels (competitors may bluff); the GameMaster records it but does not moralize. The market team is exempt (it rates honestly).

---

## 4. Visualization per Zelazny — TUI and reports

> Status: open. None of `harw-tui/src/charts.rs`, the message-title heuristic, or a dedicated matrix-game "situation" tab exist yet. This section records the intended design for future work.

### 4.1 Principles

Zelazny's chain: first settle the **message**, then the **comparison type**, then the **chart form**. A chart's title is the message, not the topic ("explorer-2 uses two-thirds of the tokens," not "token usage by agent"). He distinguishes five comparisons: share of a whole (component), ranking (item), change over time (time series), distribution across size classes (frequency), and the relationship between two variables (correlation). He rates pie charts as overrated and bar charts as underrated.

Mapped onto ratatui (`harw-tui/Cargo.toml`):

| Comparison | Signal words in the message | Zelazny form | ratatui |
|---|---|---|---|
| Component | share, % of, accounts for | pie / 100% bar | a stacked 100% bar as one line of colored `Span`s; or a `Gauge`/`LineGauge` per part |
| Item | larger, smaller, rank, tied | horizontal bars | `BarChart` with `.direction(Direction::Horizontal)`, sorted descending |
| Time series | rises, falls, fluctuates, since | columns / line | `Sparkline` (compact), `Chart` + `Dataset` with `GraphType::Line` |
| Frequency | range, concentration, distribution | column histogram | `BarChart` vertical, buckets as labels |
| Correlation | depends on, rises with | scatter | `Chart` with `GraphType::Scatter`, `Marker::Braille` |
| exact values | – | table | `Table` (numbers right-aligned, message in the block title) |

### 4.2 Intended applications

**Token usage** (`AgentMonitor::totals`, `harw_types::TokenUsage`)
- Component: "explorer children use 58% of input tokens" — one 100% line per run (root / children / UIA workers), cache share as its own component ("41% from cache").
- Item: "source-researcher is the most expensive agent" — a horizontal `BarChart`, top 8.
- Time series: a `Sparkline` of tokens/minute per agent in the agent panel (one line tall).
- Gauge: context usage against `ContextBudgetSpec` — a `Gauge` titled "context 82% — compaction due soon."

**Agent activity**
- Frequency: "most tool calls finish under 2s, five take over 30s" — a histogram of tool latencies.
- Item: tool calls per tool, horizontal.
- Correlation: "longer contexts don't correlate with more retries" — a scatter of context size × retries per turn (report/detail view only, not the live panel).

**Matrix-game world state** (a new "situation" tab)
- Component: market share per segment for the current move (a 100% line per segment, team colors).
- Time series: share/EBIT per team across moves — a `Chart` line, x = move labels.
- Item: cash ranking by move.
- `Table`: a team × KPI matrix with delta from the previous move; auto title: the biggest change.
- List (not a chart): an event timeline (injects, disclosures) by move/phase.
- Visibility: a player view shows only that player's scope (the same compartment rules as §3.3); the game-master view shows everything.

**AAR reports** (markdown, an AAR renderer)
- Every section starts with a message heading; at most one chart per message.
- Markdown charts as Unicode bars (`█▉▊▋▌▍▎▏`) plus a number table; no pies.
- Forecast error (expected vs. actual KPIs) as paired bars per team.

### 4.3 Deriving message titles automatically

A future, pure and testable module `harw-tui/src/charts.rs`:

```rust
pub enum Comparison { Component, Item, TimeSeries, Frequency, Correlation }

/// Derives a message from data; falls back to the topic title.
pub fn message_title(kind: Comparison, series: &LabeledSeries, topic: &str) -> String;
// Component: largest share ≥ 40% → "{label} accounts for {pct}%"
// Item: gap between rank 1 and rank 2 ≥ 1.5x → "{label} leads clearly"; else "… roughly tied"
// TimeSeries: slope/variance → "rising since …" | "falling" | "fluctuating"
// Frequency: modal bucket → "most values between {a} and {b}"
// Correlation: |r| < 0.2 → "no relationship between …"; else direction
```

The same function would be used by both the AAR renderer and the TUI, so the report and the screen make the same claim.

---

## 5. Status by area

| Area | Status | Notes |
|---|---|---|
| Business-mode scenario schema, phases, rules, market model | Implemented | `harw-matrix-game/src/scenario.rs`, `business.rs`, `phases.rs` |
| `report.md` / LaTeX handoff for business-mode runs | Implemented | `harw-ops/src/matrix/report.rs`, `uia-latex-writer` template `business-paper` |
| Bounded adjudicator overrides as a distinct journaled event | Open | today handled through the general effect-validation/facilitator-note path |
| Declarative `[[business.disclosure]]` rules | Open | disclosure today follows plain public/private mechanics |
| Structured channel-pattern analysis, bias checklist, `CounterApproach` taxonomy in the AAR | Open | narrative AAR text can cover this qualitatively today |
| `ResearchFinding` schema extensions (hypotheses, assumptions, indicators, source grading, likelihood) | Implemented | `harw-research/src/types.rs`, `schema.rs`, `validate.rs` |
| Six analytical skills (`analysis-ach`, `key-assumptions`, `red-team`, `indicators-warnings`, `source-grading`, `hypothesis-decomposition`) | Open | not created under `harw-home/assets/skills/` |
| `all-source-analyst` / `red-teamer` agent assets; `source-researcher` gaining `source-grading` | Open | not created under `harw-home/assets/agents/` |
| Compartmentation invariant (per-recipient context ceiling, canary-word non-leak test) | Implemented (mechanism) / Open (dedicated test) | follows from `matrix-game.md` §2's projection guarantee; a dedicated canary-word regression test for business mode is not confirmed |
| Tearline summaries, `need_to_know` section on delegation | Open | `ContextCeiling::intersect` exists; the tearline convention does not |
| `ReturnEnvelope.serendipity` | Open | not implemented |
| `harw-tui/src/charts.rs`, `message_title`, matrix-game "situation" tab | Open | not implemented |
| `attack_counter` variant with ambassador briefs | Open (scenario-level only) | the variant field is data; the ambassador-brief mechanic is not confirmed implemented |
| Indicators as knowledge-base follow-ups | Open | depends on `indicators-warnings` |

Crate and field names for the matrix-game core (`[[teams]]`, `[channels]`, `[[injects]]`, etc.) are governed by `matrix-game.md`; this document's business-mode extensions build on top of it without redefining them.
