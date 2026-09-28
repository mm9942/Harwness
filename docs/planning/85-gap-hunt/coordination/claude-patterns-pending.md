# Pattern catalog: entries not yet in `patterns.md`

`patterns.md` on the integration branch has P1–P14. The entries below were
observed during R16 and are recorded here so they reach git. Harw owns the
integration branch and decides whether they move into the catalog.

## P15: agent artefacts in code
Fixers write their brief into code comments, for example "Covers the brief's
three cases" (harw-security-hub config.rs) or "see the fixer agent's report"
(harw-authority lib.rs). A reader later finds neither the brief nor the
report.
Countermeasure: the fix rules say comments never refer to a brief, contract,
report or wave. An xtask gate greps `*.rs` for `the brief`, `Fixer-Agent`,
`fixer report` and `per the contract`.

## P16: ripple converges, but not to zero
- ripple-egress went from 11 items to 9 in round 2, almost all docs. One real
  defect in round 2 came from the ripple fix itself: an idle counter that
  counted from connection start.
- wa-web had 9 items, one of which was behavioural: the factory ran before
  the check.

Countermeasure: ripple round 2 runs as a contract wave with its own
review, and after that there is no further ripple run. Docs items are
collected, not chased.

## P17: the same solution built three times
harw-authority, harw-security-hub and harw-tool-plan each built their own
safe file open (nofollow, no FIFO, size, owner), although `harw_fsutil`
already provides it. The cause is the one-file rule: `Cargo.toml` is off
limits, so a fixer rebuilds the helper locally.
Countermeasure: the finder and fixer prompts name the workspace helpers
(`harw_fsutil::open_nofollow`/`open_beneath`, `read_capped`). When a
`Cargo.toml` change is needed, the work goes into a contract wave.

## P18: repair without a blocking finding
In ripple-authz a repair stage made a 69-line edit to
`harw-project-discovery/src/discovery.rs`. No review finding blocked on that
file. The edit was dropped and moved to wave 2.
Countermeasure (kit-4): repairs run only after `ok=false`, and only on files
declared in the contract.

## P19: the session limit is a shared point of failure
One fan-out of many parallel waves (w1–w8, w4-2, w8-2, ripple-web,
pr-baseline, blocker maps, field-report verification) hit the account
session limit at the same time. The fixers had mostly finished; the late
stages (review, repair, ripple) died. The result was many changes in the
tree with exactly the checks missing.
Countermeasures:
- Size parallelism to the token budget, not to CPUs.
- Finish one wave completely (review, commit, manifest) before starting the
  next.
- Gates fail closed when a stage returns null.

## P20: more agents is not more throughput
Every follow-up workflow (`-2`, ripple, contract-c-2) existed because its
predecessor was incomplete. Coordination cost grew faster than progress.

## Common core of P12–P20
Almost every failure sits at a handoff, not in program logic:
- resume prefix (P12);
- verifying against the fixed tree instead of the base (P13);
- a moving working directory (P14);
- the brief leaking into code (P15);
- a repair without a finding (P18);
- the session limit (P19).

Each handoff between agents is a place where state is lost or lands in the
wrong place. Harw's goal/plan model with a single writer and a frozen SHA
removes most of these handoffs.
