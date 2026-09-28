export const meta = {
  name: 'layered-wave',
  description: 'Build a feature bottom-up toward one goal: a macro plan of leaf-crate and wiring units, leaf crates level by level in dependency order, wiring last; every level is a contract wave; no builds',
  whenToUse: 'For features that add or change several crates (new leaf crates plus their wiring into assembly/CLI/TUI). Single-crate or multi-file fixes go to contract-wave; single-file findings to gap-fix.',
  phases: [
    { title: 'Macro', detail: 'one planner turns goal + design docs into leaf and wiring units with dependencies (read-only)' },
    { title: 'Leaf', detail: 'leaf-crate units level by level in dependency order, each level one contract wave' },
    { title: 'Wiring', detail: 'wiring units into composition roots after every leaf level is complete' },
    { title: 'Goal', detail: 'evidence per acceptance criterion over the whole diff against the pinned base' },
  ],
}

// Args:
//   key          label for this wave
//   root         absolute path of the checkout (a git worktree on its own branch); required (catalog P14)
//   goal, base   goal contract and pinned base, as in contract-wave (required)
//   design       repo-relative design/plan documents the macro planner reads
//   scope        free text: what is in and out of scope
//   models       'sonnet' (default) or 'tiered'
//   maxParallel  units in flight per level (default 3)
//   plan         optional ready plan ({units, decisions_needed, summary}); skips the macro agent
//   contractWave path of contract-wave.js (default: the kit's, under root)
//   decided      owner decisions already made; given to the macro planner and
//                every level's contracts as resolved (catalog P21)
//
// Unit kinds:
//   leaf    builds or changes exactly one crate (every file under `crate/`),
//           may depend only on other leaf units
//   wiring  connects finished crates in composition roots (assembly, CLI,
//           TUI, config); runs after every leaf level, may depend on anything
// Levels come from the dependency graph (Kahn); a level starts only when the
// previous one is complete, so a failed leaf never gets wired (catalog P19).
const A = args || {}
const BUILD_RULE = 'Subagents and parallel agents must **never** run `cargo` or `rustc` in any form: no `check`, `build`, `test`, `nextest`, `clippy`, `fmt`, `run`, `doc`, `deny`, and no `make` target that calls them. They only read and edit code. At the end they report which tests they added and which commands the central build must run.'
function checkRoot(root) {
  const r = String(root || '').replace(/\/+$/, '')
  if (!r) throw new Error('root is required: pass the absolute path of the checkout (catalog P14)')
  if (!/^\/[A-Za-z0-9._\/-]+$/.test(r) || r.split('/').includes('..')) {
    throw new Error('root must be a plain absolute path of [A-Za-z0-9._/-] without ".." segments')
  }
  return r
}
const ROOT = checkRoot(A.root)
const GIT = `git -C '${ROOT}'`
const idOk = s => typeof s === 'string' && /^[a-z0-9][a-z0-9._-]*$/.test(s)
function checkGoal(goal) {
  const g = goal || {}
  if (!idOk(g.id)) throw new Error('goal.id is required ([a-z0-9._-]); a writing wave serves one goal')
  if (!String(g.statement || '').trim()) throw new Error('goal.statement is required')
  const criteria = Array.isArray(g.criteria) ? g.criteria : []
  if (!criteria.length) throw new Error('goal.criteria must name at least one acceptance criterion')
  const seen = new Set()
  for (const c of criteria) {
    if (!c || !idOk(c.id) || !String(c.text || '').trim()) throw new Error('every criterion needs an id ([a-z0-9._-]) and a text')
    if (seen.has(c.id)) throw new Error(`duplicate criterion id ${c.id}`)
    seen.add(c.id)
  }
  return { id: g.id, statement: String(g.statement).trim(), criteria, invariants: (g.invariants || []).map(String) }
}
const GOAL = checkGoal(A.goal)
const BASE = String(A.base || '')
if (!/^[0-9a-f]{7,40}$/.test(BASE)) throw new Error('base is required: the pinned base commit as 7-40 lowercase hex digits')
const MODELS = A.models || 'sonnet'
if (MODELS !== 'sonnet' && MODELS !== 'tiered') throw new Error("models must be 'sonnet' or 'tiered'")
const MAX_PARALLEL = Number.isInteger(A.maxParallel) && A.maxParallel > 0 ? A.maxParallel : 3
const KEY = A.key || GOAL.id
const DECIDED = (A.decided || []).map(String).filter(d => d.trim())
const CONTRACT_WAVE = A.contractWave || `${ROOT}/docs/planning/85-gap-hunt/kit/workflows/contract-wave.js`
const norm = p => {
  const s = String(p || '').trim().replace(/^\.\//, '')
  return s.startsWith(ROOT + '/') ? s.slice(ROOT.length + 1) : s
}
const uniq = xs => [...new Set(xs)]

const PLAN = {
  type: 'object',
  properties: {
    decisions_needed: { type: 'array', items: { type: 'string' }, description: 'owner decisions that block the plan; empty when none' },
    summary: { type: 'string' },
    units: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          id: { type: 'string', description: '[a-z0-9._-], unique' },
          kind: { type: 'string', enum: ['leaf', 'wiring'] },
          crate: { type: 'string', description: 'leaf only: the crate directory, e.g. harw-tui or dod/crates/harw-dod-x (new crates included)' },
          files: { type: 'array', items: { type: 'string' }, description: 'every repo-relative file the unit may create or change, Cargo.toml included' },
          depends_on: { type: 'array', items: { type: 'string' } },
          spec: { type: 'string', description: 'what to build: public signatures verbatim, behaviour, errors, docs' },
          tests: { type: 'array', items: { type: 'string' } },
          criteria: { type: 'array', items: { type: 'string' }, description: 'goal criterion ids this unit serves' },
          security: { type: 'boolean', description: 'true when the unit touches a trust boundary (regression test required)' },
        },
        required: ['id', 'kind', 'files', 'depends_on', 'spec', 'tests', 'criteria'],
      },
    },
  },
  required: ['decisions_needed', 'summary', 'units'],
}
const COVERAGE = {
  type: 'object',
  properties: {
    criteria: { type: 'array', items: { type: 'object', properties: { id: { type: 'string' }, status: { type: 'string', enum: ['met', 'not-met', 'unknown'] }, evidence: { type: 'string' } }, required: ['id', 'status', 'evidence'] } },
    deltas: { type: 'array', items: { type: 'string' } },
  },
  required: ['criteria', 'deltas'],
}

// Checks a plan before any coder starts. Returns the leaf and wiring levels
// or the list of errors; an invalid plan never runs (fail closed).
function validatePlan(plan) {
  const errors = []
  const units = ((plan && plan.units) || []).map(u => ({
    ...u,
    files: uniq((u.files || []).map(norm)),
    depends_on: uniq(u.depends_on || []),
    crate: u.crate ? norm(u.crate).replace(/\/+$/, '') : '',
  }))
  if (!units.length) errors.push('plan has no units')
  const byId = new Map()
  for (const u of units) {
    if (!idOk(u.id)) errors.push(`unit id ${JSON.stringify(u.id)} is not [a-z0-9._-]`)
    else if (byId.has(u.id)) errors.push(`duplicate unit id ${u.id}`)
    else byId.set(u.id, u)
    if (u.kind !== 'leaf' && u.kind !== 'wiring') errors.push(`${u.id}: kind must be leaf or wiring`)
    if (!u.files.length) errors.push(`${u.id}: no files`)
    for (const f of u.files) if (f.startsWith('/') || f.split('/').includes('..')) errors.push(`${u.id}: file ${f} is not repo-relative`)
    if (u.kind === 'leaf') {
      if (!u.crate) errors.push(`${u.id}: a leaf unit names its crate`)
      else for (const f of u.files) if (!f.startsWith(u.crate + '/')) errors.push(`${u.id}: leaf file ${f} is outside crate ${u.crate}`)
    }
  }
  const owner = new Map()
  for (const u of units) for (const f of u.files) {
    if (owner.has(f) && owner.get(f) !== u.id) errors.push(`file ${f} is in units ${owner.get(f)} and ${u.id}`)
    else owner.set(f, u.id)
  }
  const leafCrates = new Map()
  for (const u of units.filter(u => u.kind === 'leaf')) {
    if (leafCrates.has(u.crate)) errors.push(`crate ${u.crate} is split across leaf units ${leafCrates.get(u.crate)} and ${u.id}`)
    else leafCrates.set(u.crate, u.id)
  }
  for (const u of units) for (const d of u.depends_on) {
    if (!byId.has(d)) errors.push(`${u.id}: depends on unknown unit ${d}`)
    else if (u.kind === 'leaf' && byId.get(d).kind !== 'leaf') errors.push(`${u.id}: a leaf unit cannot depend on wiring unit ${d}`)
  }
  const known = new Set(GOAL.criteria.map(c => c.id))
  for (const u of units) for (const c of u.criteria || []) if (!known.has(c)) errors.push(`${u.id}: unknown criterion ${c}`)
  for (const c of GOAL.criteria) if (!units.some(u => (u.criteria || []).includes(c.id))) errors.push(`criterion ${c.id} has no unit`)
  const levelsOf = kind => {
    const members = units.filter(u => u.kind === kind && byId.get(u.id) === u)
    const pending = new Map(members.map(u => [u.id, new Set(u.depends_on.filter(d => byId.has(d) && byId.get(d).kind === kind))]))
    const levels = []
    while (pending.size) {
      const ready = [...pending].filter(([, deps]) => deps.size === 0).map(([id]) => id)
      if (!ready.length) { errors.push(`${kind} units form a cycle: ${[...pending.keys()].join(', ')}`); break }
      levels.push(ready.map(id => byId.get(id)))
      for (const id of ready) pending.delete(id)
      for (const deps of pending.values()) for (const id of ready) deps.delete(id)
    }
    return levels
  }
  const leafLevels = levelsOf('leaf')
  const wiringLevels = levelsOf('wiring')
  return { errors, units, leafLevels, wiringLevels }
}

const specFinding = u => ({
  file: u.files[0],
  other_files: u.files.slice(1),
  line: 1,
  severity: u.security ? 'high' : 'medium',
  pattern: u.kind === 'leaf' ? 'BUILD-LEAF' : 'BUILD-WIRING',
  title: `${u.kind === 'leaf' ? 'Build leaf crate unit' : 'Wire unit'} ${u.id}${u.crate ? ` (${u.crate})` : ''}`,
  description: u.spec,
  evidence: `macro plan of ${KEY}; serves ${(u.criteria || []).join(', ')}`,
  fix: u.spec,
  test_idea: (u.tests || []).join('; ') || '-',
})

// A level runs as one contract wave. Its goal carries one criterion per unit
// (the unit is delivered as specified); the real acceptance criteria are
// checked once over the whole diff at the end.
async function runLevel(label, level) {
  const result = await workflow({ scriptPath: CONTRACT_WAVE }, {
    key: `${KEY}-${label}`,
    root: ROOT,
    base: BASE,
    models: MODELS,
    maxParallel: MAX_PARALLEL,
    decided: DECIDED,
    goal: {
      id: `${GOAL.id}.${label}`.toLowerCase(),
      statement: `${GOAL.statement} (level ${label}: ${level.map(u => u.id).join(', ')})`,
      criteria: level.map(u => ({ id: u.id, text: `Unit ${u.id} is delivered as specified in its own files (${u.files.join(', ')}): ${u.spec}${u.kind === 'leaf' ? ' Workspace membership, ring entries and callers are delivered by a later wiring level; do not count their absence against this unit.' : ''}` })),
      invariants: GOAL.invariants,
    },
    clusters: level.map(u => ({ id: u.id, files: u.files, findings: [specFinding(u)] })),
  })
  return { label, units: level.map(u => u.id), complete: !!(result && result.complete), result }
}

phase('Macro')
let plan = A.plan || null
if (!plan) {
  // A planner that never produces a valid plan ends the wave as no-plan
  // instead of aborting the workflow.
  plan = await agent(`Repository root: ${ROOT} (a git checkout on its own branch). Read files only under it; run git as \`${GIT} …\`. Do not edit anything. ${BUILD_RULE}

You are the macro planner. Goal ${GOAL.id}: ${GOAL.statement}
Pinned base: ${BASE}.
Acceptance criteria:
${GOAL.criteria.map(c => `- ${c.id}: ${c.text}`).join('\n')}
Invariants:
${GOAL.invariants.map(i => `- ${i}`).join('\n') || '- none'}
Design documents: ${(A.design || []).join(', ') || 'none named; find them'}
Scope: ${A.scope || 'as the goal says'}
${DECIDED.length ? `Owner decisions already made (resolved; plan with them, never list them under decisions_needed again):\n${DECIDED.map(d => `- ${d}`).join('\n')}\n` : ''}
Turn this into units that one contract wave each can build:
- Prefer libraries: put new logic into leaf library crates (lib.rs only, no binary, as little I/O as possible, pure and table-testable), depending only on lower rings; the wiring units call them from the existing crates, which keep I/O, rendering and configuration.
- leaf: exactly one crate (every file under its directory, Cargo.toml included), bottom-up. A new crate is a leaf unit whose files include its Cargo.toml and src/lib.rs; the root Cargo.toml membership edit belongs to a wiring unit. A leaf may depend only on other leaf units.
- wiring: edits in composition roots (harw-runtime assembly, harw-cli, harw-tui, harw-config, root Cargo.toml, xtask/arch-policy.toml) that connect finished leaf crates. A new library crate needs a wiring unit for its root Cargo.toml membership and its ring entry in xtask/arch-policy.toml.
- One crate in at most one leaf unit; every file in exactly one unit; no cycles.
- spec: public signatures verbatim, behaviour, error variants, doc updates, ring rules from xtask/arch-policy.toml; tests: concrete test names/ideas in the unit's own files.
- Every acceptance criterion is served by at least one unit (criteria ids); mark units at a trust boundary security=true.
- If the goal needs an owner decision (UX, security trade-off, new dependency), list it in decisions_needed and plan nothing that depends on it.`,
    { label: `macro:${KEY}`, phase: 'Macro', schema: PLAN, model: MODELS === 'tiered' ? 'opus' : 'sonnet', agentType: 'focused-explorer' })
    .catch(error => { log(`${KEY}: macro planner failed: ${String(error && error.message || error).slice(0, 200)}`); return null })
}
if (!plan) {
  log(`${KEY}: NOT complete; no macro plan`)
  return { key: KEY, goal: GOAL.id, base: BASE, achieved: false, achieve: 'human-only', complete: false, status: 'no-plan' }
}
if ((plan.decisions_needed || []).length) {
  log(`${KEY}: NOT complete; ${plan.decisions_needed.length} owner decision(s) needed`)
  return { key: KEY, goal: GOAL.id, base: BASE, achieved: false, achieve: 'human-only', complete: false, status: 'needs-decision', decisions: plan.decisions_needed, plan }
}
const checked = validatePlan(plan)
if (checked.errors.length) {
  log(`${KEY}: NOT complete; plan rejected (${checked.errors.length} error(s))`)
  return { key: KEY, goal: GOAL.id, base: BASE, achieved: false, achieve: 'human-only', complete: false, status: 'plan-invalid', errors: checked.errors, plan }
}
log(`${KEY}: ${checked.leafLevels.length} leaf level(s), ${checked.wiringLevels.length} wiring level(s)`)

const levels = []
let stopped = null
phase('Leaf')
for (let i = 0; i < checked.leafLevels.length && !stopped; i++) {
  const run = await runLevel(`L${i + 1}`, checked.leafLevels[i])
  levels.push(run)
  if (!run.complete) stopped = run.label
}
if (!stopped) {
  phase('Wiring')
  for (let i = 0; i < checked.wiringLevels.length && !stopped; i++) {
    const run = await runLevel(`W${i + 1}`, checked.wiringLevels[i])
    levels.push(run)
    if (!run.complete) stopped = run.label
  }
}

// The real acceptance criteria, once, over every file the plan owns.
let coverage = GOAL.criteria.map(c => ({ id: c.id, status: 'unknown', evidence: '', reason: stopped ? `stopped at level ${stopped}` : 'no goal check' }))
let deltas = []
if (!stopped) {
  phase('Goal')
  const files = uniq(checked.units.flatMap(u => u.files))
  const answer = await agent(`Repository root: ${ROOT}. Do not edit. ${BUILD_RULE}

You check wave ${KEY} against its goal. Goal ${GOAL.id}: ${GOAL.statement}
Read \`${GIT} diff ${BASE} -- ${files.join(' ')}\` and the tests in it. New files are untracked and do not appear in git diff: list them with \`${GIT} status --short --untracked-files=all\` and read them directly (catalog P23).
For every acceptance criterion answer met, not-met or unknown, using exactly these ids and no others: ${GOAL.criteria.map(c => c.id).join(', ')}; met needs evidence: a repo-relative file:line or a test name.
${GOAL.criteria.map(c => `- ${c.id}: ${c.text}`).join('\n')}
List under deltas every contradiction between code/tests/gates and planning docs.`,
    { label: `goal:${KEY}`, phase: 'Goal', schema: COVERAGE, model: 'sonnet', agentType: 'focused-explorer' })
  const got = new Map(((answer && answer.criteria) || []).map(c => [c.id, c]))
  coverage = GOAL.criteria.map(c => {
    const a = got.get(c.id)
    if (!a) return { id: c.id, status: 'unknown', evidence: '', reason: answer ? 'not answered' : 'no goal check' }
    const met = a.status === 'met' && String(a.evidence || '').trim() !== ''
    return { id: c.id, status: met ? 'met' : (a.status === 'met' ? 'unknown' : a.status), evidence: String(a.evidence || '') }
  })
  deltas = uniq([...((answer && answer.deltas) || []), ...levels.flatMap(l => (l.result && l.result.deltas) || [])])
}
const complete = !stopped && coverage.every(c => c.status === 'met')
log(`${KEY}: ${complete ? 'complete' : 'NOT complete'}; ${levels.filter(l => l.complete).length}/${checked.leafLevels.length + checked.wiringLevels.length} levels done${stopped ? `, stopped at ${stopped}` : ''}, goal ${GOAL.id} ${coverage.filter(c => c.status === 'met').length}/${GOAL.criteria.length} criteria met`)
return { key: KEY, goal: GOAL.id, base: BASE, achieved: false, achieve: 'human-only', complete, status: stopped ? 'stopped' : (complete ? 'complete' : 'goal-not-met'), stoppedAt: stopped, plan: checked.units, levels, coverage, deltas }
