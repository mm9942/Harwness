# Harw Work Packet — Research Paper Draft

## Purpose

Use Harw's own authoring capabilities to turn the current paper workstream into a reviewed research-paper draft **inside this draft PR**.

This is not a publication command. The PR stays draft. No merge, tag, arXiv submission, Hugging Face publication, or scientific claim is authorized by this work packet.

## Working branch and evidence baseline

- Working branch: `paper/harw-runtime-paper`
- Initial evidence baseline: `main@442cf92e3e79f0a6fd31e650a55e8be676c47f1e`
- Existing inputs:
  - `paper/README.md`
  - `paper/OUTLINE.md`
  - `paper/EVIDENCE_LEDGER.md`
  - `paper/PUBLISHING.md`

The baseline above defines the initial implementation evidence. If the branch later rebases or deliberately selects a newer Harw baseline, record that change explicitly in the evidence ledger before using newer behavior as evidence.

## Required Harw authoring pipeline

Use the bundled author/review separation already defined by Harw:

```
root / orchestrator
       |
       +--> business-author
       |       skills:
       |       - business-writing-pyramid
       |       - author-review-pipeline
       |
       +--> business-reviewer
       |       skills:
       |       - author-review-pipeline
       |       - business-writing-pyramid
       |
       +--> business-author
       |       revision from review findings
       |
       +--> business-reviewer
       |       final logical review
       |
       '--> latex-writer
               skills:
               - latex-report
               - latex-writing
               - xelatex-compile
```

The writer must not approve its own text. The reviewer must not silently rewrite it. The LaTeX stage must not change the approved argument structure.

## Language and audience

- Paper language: **English**
- Audience: researchers and engineers working on agentic AI systems, model/runtime architecture, long-running agents, context engineering, memory, tool execution, and agent evaluation.
- Desired reader action: understand Harw's systems thesis, inspect the released implementation/evidence, and be able to reproduce the reported experiments.
- Tone: technical research paper; precise, restrained, evidence-first; no marketing language.

## Core reader question

> Which responsibilities of a persistent agent can be represented as explicit, typed, capability-bounded runtime mechanisms rather than being repeatedly inferred from prompts, and what measurable effects does that have on reliability, context use, authority enforcement, recovery, and long-horizon execution?

## Provisional answer

Use this only as a hypothesis/storyline seed, not as a proven result:

> Harw treats substantial parts of agent behavior — context selection, durable knowledge, authority, tool placement, session state, orchestration, and verification — as explicit runtime structure around interchangeable language models; the paper evaluates which of these mechanisms produce measurable systems benefits.

Anything after the semicolon remains a research objective until experiments establish it.

---

# Stage A — Storyline

Spawn `business-author` for **Storyline only**.

Input:

- the four existing `paper/*.md` files;
- the relevant current code/tests/docs required to understand claims C01–C06;
- no unverified external literature;
- no conversational claims unless they are independently represented in repository evidence.

Required output:

`paper/drafts/storyline-v1.md`

Use the author's normal Storyline format and include:

1. reader;
2. desired reader action;
3. Situation / Complication / Question;
4. one top-line answer;
5. 3–5 mutually distinct supporting claims;
6. evidence underneath each supporting claim;
7. explicit evidence gaps;
8. strongest counterargument;
9. limitations;
10. what the paper does **not** claim.

Important:

- A source-code path proves implementation, not superiority.
- A design document proves intent, not implementation.
- A test proves tested behavior, not general empirical benefit.
- Any comparative wording such as *better*, *safer*, *faster*, *more reliable*, *efficient*, *robust*, or *improves* must remain a hypothesis until a defined experiment supports it.
- Model-Turn-Chain, harness-to-training feedback, and other not-yet-evaluated architecture stay in Future Work.

# Stage B — Storyline review

Spawn `business-reviewer` on the complete Storyline package.

Required output:

`paper/reviews/storyline-v1-review.md`

The reviewer must use its standard structured finding format.

Extra review criteria for this paper:

- Is each novelty/contribution statement narrower than the evidence?
- Is CURRENT separated from DESIGNED/FUTURE?
- Are systems analogies to model architecture clearly presented as analogies rather than identity?
- Is there accidental novelty-by-combination language?
- Is there a falsifiable research question?
- Can every implementation claim be tied to a code/test anchor?
- Are empirical claims reserved for the experiment section?

If any `muss` finding remains, return to `business-author`.

Maximum author/reviewer revision cycles before escalating: **2**.

Write revisions as:

- `paper/drafts/storyline-v2.md`
- `paper/reviews/storyline-v2-review.md`

Do not delete prior versions.

# Stage C — Evidence-bound manuscript draft

Only after the Storyline has no open `muss` findings, spawn `business-author` again for the manuscript.

Required output:

`paper/drafts/manuscript-v1.md`

Required sections:

1. Abstract
2. Introduction
3. Research Questions
4. Background and Related Work
5. Harw Design Principles
6. Architecture
   - typed agent/context construction
   - bounded context and knowledge
   - authority/tool execution
   - durable sessions/state
   - multi-agent execution and verification
7. Experimental Method
8. Results
9. Discussion
10. Limitations / Threats to Validity
11. Future Work
12. Artifact Availability / Reproducibility

Rules:

### Abstract

Until primary experiments exist, the abstract must not pretend results exist. Use explicit provisional language or mark result sentences with `[RESULT PENDING]`.

### Related Work

The business writer has no web authority and must not manufacture citations.

If vetted literature material has not been supplied, write the structure and mark entries:

`[LITERATURE NEEDED: <question/category>]`

Do not invent authors, titles, arXiv IDs, years, venues, or BibTeX.

### Architecture

For each substantive implementation statement, include an inline source anchor in draft form:

`[EVIDENCE: path | test | claim-id]`

These markers can be converted to formal citations later.

### Experiments and Results

Do not fabricate results.

Use the experiment definitions from `paper/OUTLINE.md` and the evidence ledger. Missing outputs stay visibly marked:

`[EXPERIMENT PENDING: E1]`

### Claims

All claims must map to `paper/EVIDENCE_LEDGER.md`.

If the manuscript needs a claim that is not in the ledger:

1. add a candidate claim to the ledger;
2. give it the correct non-measured status;
3. only then use it in the draft.

# Stage D — Manuscript review

Run `business-reviewer` over the complete manuscript package.

Required output:

`paper/reviews/manuscript-v1-review.md`

In addition to its normal rubric, review:

- claim/evidence alignment;
- contribution inflation;
- causal language;
- use of evaluative/comparative adjectives;
- missing baselines;
- unsupported generalization from one provider/model;
- reproducibility gaps;
- CURRENT vs DESIGNED vs FUTURE;
- clarity for a reader who has never seen Harw.

Then return findings to `business-author`.

Maximum manuscript revision cycles before escalating: **2**.

Preserve every review file.

# Stage E — LaTeX research draft

Only after the manuscript has no open `muss` findings, spawn `latex-writer`.

Important: this is a **scientific paper**, not automatically the bundled business-paper visual template. Use the LaTeX writer's scientific-paper capability. Do not force the business-paper report styling into an arXiv manuscript.

Until a target venue/template is selected:

- use a conservative article/KOMA-style source structure supported by the writer;
- keep the manuscript modular;
- do not invent author metadata;
- author field: `% TODO: author metadata` unless explicitly present in this work packet or a later human instruction;
- bibliography contains only verified references supplied by a research/literature stage.

Required target layout:

```
paper/src/
├── main.tex
├── sections/
│   ├── introduction.tex
│   ├── related-work.tex
│   ├── architecture.tex
│   ├── methodology.tex
│   ├── results.tex
│   ├── discussion.tex
│   └── limitations.tex
└── references.bib
```

The LaTeX writer must:

1. statically check every edited source;
2. use `latex.check`;
3. use `latex.build` only through its typed tool;
4. perform at most the configured correction rounds;
5. inspect a PDF sample if a PDF was produced;
6. keep `% TODO:` for unresolved evidence/citation/result gaps;
7. report build warnings honestly.

Do not publish the generated PDF from this work packet.

# Stage F — Evidence reconciliation

After the manuscript/LaTeX pass, the root/orchestrator must reconcile:

- `paper/EVIDENCE_LEDGER.md`
- manuscript claims
- code/test anchors
- experiment placeholders
- literature placeholders

Produce:

`paper/RESEARCH_STATUS.md`

Required status groups:

### Ready as implementation description

Claims that are adequately anchored in current code/tests.

### Needs experiment

Claims that require E1–E5 or another defined experiment.

### Needs literature

Related-work or novelty questions requiring external research.

### Designed / future only

Architecture that must not appear as a current result.

### Blocking publication

Anything that would make a public preprint misleading or irreproducible.

# Stop conditions

Stop and report instead of inventing when:

- a required source cannot be found;
- implementation and planning docs contradict each other;
- a cited test does not prove the claimed property;
- literature has not been supplied;
- an experiment result does not exist;
- author/affiliation metadata is unknown;
- LaTeX is unavailable;
- the reviewer still has open `muss` findings after two cycles.

# Completion criteria for this draft PR

This work packet is complete when the branch contains:

- reviewed Storyline;
- evidence-bound manuscript draft;
- structured review reports;
- modular LaTeX source if build tooling is available;
- updated evidence ledger;
- `RESEARCH_STATUS.md`;
- all missing literature/results left explicit rather than invented.

The PR remains **Draft** after completion.

A human decides when the scientific claims are strong enough for experiments to be frozen, the PR to leave Draft, and any artifact to be published.
