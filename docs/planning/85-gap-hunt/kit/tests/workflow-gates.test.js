// Gate tests for the kit workflows, run with plain `node` (no dependencies):
//   node docs/planning/85-gap-hunt/kit/tests/workflow-gates.test.js
// Each workflow runs against a fake agent() that returns canned results, so the
// tests check the deterministic control flow only: root validation, the ripple
// gate, the contract file-set check and when repairs may run.
const fs = require('fs')
const K = require('path').join(__dirname, '..', 'workflows') + '/'
function load(f) { return fs.readFileSync(K + f, 'utf8').replace(/^export const meta/m, 'const meta') }
// Every writing wave needs a goal and a pinned base; tests that are not about
// them get these defaults, and a goal check that answers every criterion.
// Pass `goal: null` / `base: null` to test the checks, and raw=true to answer
// the goal check yourself.
const GOAL = { id: 'g1', statement: 's', criteria: [{ id: 'c1', text: 'criterion' }] }
const BASE = 'a856dea'
const MET = { criteria: [{ id: 'c1', status: 'met', evidence: 'a/src/x.rs:1' }], deltas: [] }
async function run(f, args, respond, raw) {
  if (args && !('goal' in args)) args = { ...args, goal: GOAL }
  if (args && !('base' in args)) args = { ...args, base: BASE }
  const inner = respond
  if (!raw) respond = async (l, p) => { const v = await inner(l, p); return v == null && l.startsWith('goal:') ? MET : v }
  const calls = [], models = []
  let peak = 0, live = 0
  const agent = async (prompt, opts) => {
    calls.push(opts.label); models.push([opts.label, opts.model]); live++; peak = Math.max(peak, live)
    try { await new Promise(r => setTimeout(r, 1)); return await respond(opts.label, prompt) } finally { live-- }
  }
  const parallel = async ts => Promise.all(ts.map(t => t().catch(() => null)))
  const pipeline = async (items, ...stages) => Promise.all(items.map(async (it, i) => { let v = it; for (const s of stages) { try { v = await s(v, it, i) } catch (e) { return null } } return v }))
  // A nested workflow() call is answered like an agent labelled workflow:<key>.
  const workflow = async (ref, a) => { calls.push(`workflow:${a && a.key}`); return respond(`workflow:${a && a.key}`, a) }
  const fn = new Function('args', 'agent', 'parallel', 'pipeline', 'phase', 'log', 'workflow', 'return (async()=>{' + load(f) + '})()')
  try { const r = await fn(args, agent, parallel, pipeline, () => {}, () => {}, workflow); return { r, calls, models, peak } } catch (e) { return { err: e.message, calls, models, peak } }
}
const run2 = (...a) => run(...a)
const R = '/tmp/x/repo'
const ok = (name, cond) => { console.log((cond ? 'PASS ' : 'FAIL ') + name); if (!cond) process.exitCode = 1 }
;(async () => {
  // root validation
  for (const bad of [undefined, 'rel/path', "/tmp/a'b", '/tmp/a b', '/tmp/../etc', '/tmp/$(x)']) {
    const { err } = await run('gap-fix.js', { root: bad, findings: [] }, () => null)
    ok(`gap-fix rejects root ${JSON.stringify(bad)}`, !!err)
    const c = await run('contract-wave.js', { root: bad, clusters: [] }, () => null)
    ok(`contract-wave rejects root ${JSON.stringify(bad)}`, !!c.err)
  }
  const good = await run('gap-fix.js', { root: R + '/', findings: [] }, () => null)
  ok('gap-fix accepts trailing slash root', !good.err)
  // ripple three-state
  const F = [{ file: R + '/a/src/x.rs', line: 1, severity: 'low', title: 't', description: 'd', evidence: 'e', fix: 'f' }]
  const fixr = { file: 'a/src/x.rs', fixed: ['t'], skipped: [], tests_added: [] }
  const base = l => l.startsWith('fix:') ? fixr : l.startsWith('review:') ? { ok: true, problems: [] } : null
  let o = await run('gap-fix.js', { root: R, key: 'k', findings: F }, l => l.startsWith('ripple:') ? { items: [], summary: 'clean' } : base(l))
  ok('ripple clear -> complete', o.r.complete === true && o.r.rippleStatus === 'clear')
  ok('path inside root normalized', o.r.files[0].file === 'a/src/x.rs')
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F }, l => l.startsWith('ripple:') ? { items: [{ file: R + '/b/src/y.rs', line: 3, problem: 'p' }], summary: '' } : base(l))
  ok('ripple findings -> not complete', o.r.complete === false && o.r.rippleStatus === 'findings')
  ok('ripple item id and normalized file', o.r.ripple[0].id === 'k-R1' && o.r.ripple[0].file === 'b/src/y.rs')
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F }, l => l.startsWith('ripple:') ? null : base(l))
  ok('ripple missing -> not complete', o.r.complete === false && o.r.rippleStatus === 'missing')
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F, ripple: false }, base)
  ok('ripple skipped -> complete', o.r.complete === true && o.r.rippleStatus === 'skipped')
  // contract file set
  const C = { id: 'c1', files: ['a/src/x.rs', 'b/src/y.rs', 'a/src/x.rs'], findings: [{ file: 'a/src/x.rs', other_files: ['z/src/w.rs'], line: 1, severity: 'low', pattern: 'M1', title: 't', description: 'd', evidence: 'e', fix: 'f' }] }
  const entry = (file, change = true) => ({ file, change, instructions: 'do', tests: [] })
  const cw = contractFiles => async (l, p) => {
    if (l.startsWith('contract:')) return { feasible: true, decisions_needed: [], summary: 's', files: contractFiles, _p: p }
    if (l.startsWith('code:')) return { file: l.slice(5), done: ['x'], deviations: [], tests_added: [] }
    if (l.startsWith('review:')) return { ok: true, problems: [] }
    return null
  }
  let c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C] }, cw([entry('a/src/x.rs')]))
  ok('omitted declared file -> contract-mismatch, no coder', c.r.reports[0].status === 'contract-mismatch' && c.r.reports[0].omitted.includes('b/src/y.rs') && !c.calls.some(x => x.startsWith('code:')) && c.r.complete === false)
  c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C] }, cw([entry('a/src/x.rs'), entry('b/src/y.rs'), entry('z/src/w.rs')]))
  ok('extra file -> contract-mismatch', c.r.reports[0].status === 'contract-mismatch' && c.r.reports[0].extra.includes('z/src/w.rs'))
  c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C] }, cw([entry('a/src/x.rs'), entry(R + '/a/src/x.rs'), entry('b/src/y.rs')]))
  ok('duplicate entry (abs+rel) -> contract-mismatch', c.r.reports[0].status === 'contract-mismatch' && c.r.reports[0].duplicates.includes('a/src/x.rs'))
  c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C] }, cw([]))
  ok('empty contract list -> contract-mismatch', c.r.reports[0].status === 'contract-mismatch')
  c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C] }, cw([entry(R + '/a/src/x.rs'), entry('b/src/y.rs', false)]))
  ok('exact set with one unchanged file -> ok, one coder', c.r.reports[0].status === 'ok' && c.calls.filter(x => x.startsWith('code:')).length === 1 && c.r.complete === true)
  ok('uncovered finding file reported', (c.r.reports[0].uncovered || []).includes('z/src/w.rs'))
  let prompt = ''
  await run('contract-wave.js', { root: R, key: 'w', clusters: [C] }, async (l, p) => { if (l.startsWith('contract:')) prompt = p; return null })
  ok('contract prompt fences off uncovered file', prompt.includes('z/src/w.rs') && prompt.includes('Plan no edit there'))
  ok("git snippet single-quotes root", prompt.includes(`git -C '${R}'`))
})()
;(async () => {
  const R = '/tmp/x/repo'
  const ok = (name, cond) => { console.log((cond ? 'PASS ' : 'FAIL ') + name); if (!cond) process.exitCode = 1 }
  const C = { id: 'c2', files: ['a/src/x.rs'], findings: [{ file: 'a/src/x.rs', line: 1, severity: 'low', pattern: 'M1', title: 't', description: 'd', evidence: 'e', fix: 'f' }] }
  const mk = review => async l => {
    if (l.startsWith('contract:')) return { feasible: true, decisions_needed: [], summary: 's', files: [{ file: 'a/src/x.rs', change: true, instructions: 'i', tests: [] }] }
    if (l.startsWith('code:') || l.startsWith('repair:')) return { file: 'a/src/x.rs', done: ['x'], deviations: [], tests_added: [] }
    if (l.startsWith('review:')) return review
    if (l.startsWith('rereview:')) return { ok: true, problems: [] }
    return null
  }
  let c = await run2('contract-wave.js', { root: R, key: 'w', clusters: [C] }, mk({ ok: true, problems: [{ file: 'q/src/other.rs', problem: 'REPORT ONLY' }] }))
  ok('ok=true with listed problems -> ok, no repair', c.r.reports[0].status === 'ok' && !c.calls.some(x => x.startsWith('repair:')))
  c = await run2('contract-wave.js', { root: R, key: 'w', clusters: [C] }, mk({ ok: false, problems: [{ file: 'q/src/other.rs', problem: 'caller breaks' }] }))
  ok('ok=false only outside -> unresolved, no repair outside', c.r.reports[0].status === 'unresolved' && !c.calls.some(x => x.startsWith('repair:')) && c.r.reports[0].outside[0].file === 'q/src/other.rs')
  c = await run2('contract-wave.js', { root: R, key: 'w', clusters: [C] }, mk({ ok: false, problems: [{ file: R + '/a/src/x.rs', problem: 'bug' }] }))
  ok('ok=false inside -> repair + rereview -> repaired', c.r.reports[0].status === 'repaired' && c.calls.includes('repair:a/src/x.rs') && c.calls.includes('rereview:c2'))
})()
;(async () => {
  // Goal contract: goal, criteria and a pinned base are required.
  const R = '/tmp/x/repo'
  const ok = (name, cond) => { console.log((cond ? 'PASS ' : 'FAIL ') + name); if (!cond) process.exitCode = 1 }
  const bad = [
    ['no goal', { goal: null }],
    ['no criteria', { goal: { id: 'g', statement: 's', criteria: [] } }],
    ['criterion without text', { goal: { id: 'g', statement: 's', criteria: [{ id: 'c1', text: '' }] } }],
    ['duplicate criterion', { goal: { id: 'g', statement: 's', criteria: [{ id: 'c1', text: 't' }, { id: 'c1', text: 'u' }] } }],
    ['no base', { base: null }],
    ['base not hex', { base: 'dev' }],
    ['unknown models', { models: 'haiku' }],
  ]
  for (const [name, extra] of bad) {
    const g = await run('gap-fix.js', { root: R, findings: [], ...extra }, () => null)
    ok(`gap-fix rejects ${name}`, !!g.err)
    const c = await run('contract-wave.js', { root: R, clusters: [], ...extra }, () => null)
    ok(`contract-wave rejects ${name}`, !!c.err)
  }
  const F = sev => [{ file: 'a/src/x.rs', line: 1, severity: sev, title: 't', description: 'd', evidence: 'e', fix: 'f' }]
  const fixr = { file: 'a/src/x.rs', fixed: ['t'], skipped: [], tests_added: ['test_x'] }
  const answer = (goal, review) => l => l.startsWith('fix:') ? fixr : l.startsWith('review:') ? review : l.startsWith('ripple:') ? { items: [], summary: 'clean' } : l.startsWith('goal:') ? goal : null
  const good = { ok: true, problems: [] }
  let o = await run('gap-fix.js', { root: R, key: 'k', findings: F('low') }, answer(null, good), true)
  ok('goal check missing -> not complete, criterion unknown', o.r.complete === false && o.r.coverage[0].status === 'unknown')
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F('low') }, answer({ criteria: [{ id: 'c1', status: 'met', evidence: ' ' }], deltas: [] }, good), true)
  ok('met without evidence -> not complete', o.r.complete === false && o.r.coverage[0].reason === 'met without evidence')
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F('low') }, answer({ criteria: [{ id: 'c1', status: 'not-met', evidence: '' }], deltas: ['plan says X, code does Y'] }, good), true)
  ok('not-met -> not complete, delta kept', o.r.complete === false && o.r.deltas.includes('plan says X, code does Y'))
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F('low') }, answer({ criteria: [{ id: 'c1', status: 'met', evidence: 'a/src/x.rs:1' }, { id: 'zz', status: 'met', evidence: 'x' }], deltas: [] }, good), true)
  ok('all met -> complete, never achieved', o.r.complete === true && o.r.achieved === false && o.r.achieve === 'human-only' && o.r.coverage.length === 1 && o.r.goal === 'g1' && o.r.base === 'a856dea')
  // Security invariant: a high finding needs a named regression test.
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F('high') }, answer(null, good))
  ok('high finding without regression test -> unverified', o.r.complete === false && o.r.open[0].status === 'unverified')
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F('high') }, answer(null, { ok: true, problems: [], regression_test: 'test_x' }))
  ok('high finding with regression test -> ok', o.r.complete === true && o.r.files[0].status === 'ok')
  // Models: Sonnet 5 by default, Opus only when tiered.
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F('high') }, answer(null, good))
  ok('default: every agent on sonnet', o.models.length > 0 && o.models.every(([, m]) => m === 'sonnet'))
  o = await run('gap-fix.js', { root: R, key: 'k', findings: F('high'), models: 'tiered' }, answer(null, good))
  ok('tiered: high fix and ripple on opus', o.models.some(([l, m]) => l.startsWith('fix:') && m === 'opus') && o.models.some(([l, m]) => l.startsWith('ripple:') && m === 'opus'))
  // maxParallel caps files in flight (catalog P19).
  const many = Array.from({ length: 7 }, (_, i) => ({ file: `a/src/f${i}.rs`, line: 1, severity: 'low', title: 't', description: 'd', evidence: 'e', fix: 'f' }))
  const anyFile = l => l.startsWith('fix:') ? { file: l.slice(4), fixed: ['t'], skipped: [], tests_added: [] } : l.startsWith('review:') ? good : l.startsWith('ripple:') ? { items: [], summary: '' } : null
  o = await run('gap-fix.js', { root: R, key: 'k', findings: many, maxParallel: 2 }, anyFile)
  ok('maxParallel 2 -> at most 2 agents in flight, all files done', o.peak <= 2 && o.r.files.length === 7 && o.r.complete === true)
  // contract-wave: goal gate and security gate.
  const C = sev => ({ id: 'c9', files: ['a/src/x.rs'], findings: [{ file: 'a/src/x.rs', line: 1, severity: sev, pattern: 'M1', title: 't', description: 'd', evidence: 'e', fix: 'f' }] })
  const cw = (review, goal) => async l => {
    if (l.startsWith('contract:')) return { feasible: true, decisions_needed: [], summary: 's', files: [{ file: 'a/src/x.rs', change: true, instructions: 'i', tests: [] }] }
    if (l.startsWith('code:')) return { file: 'a/src/x.rs', done: ['x'], deviations: [], tests_added: [], deltas: ['doc drift'] }
    if (l.startsWith('review:')) return review
    if (l.startsWith('goal:')) return goal
    return null
  }
  let c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C('low')] }, cw(good, null), true)
  ok('contract-wave: goal check missing -> not complete', c.r.complete === false && c.r.reports[0].status === 'ok')
  c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C('low')] }, cw(good, MET), true)
  ok('contract-wave: all met -> complete, deltas collected, never achieved', c.r.complete === true && c.r.deltas.includes('doc drift') && c.r.achieved === false)
  c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C('critical')] }, cw(good, MET), true)
  ok('contract-wave: critical without regression test -> unverified', c.r.complete === false && c.r.reports[0].status === 'unverified')
  c = await run('contract-wave.js', { root: R, key: 'w', clusters: [C('critical')] }, cw({ ok: true, problems: [], regression_test: 'test_y' }, MET), true)
  ok('contract-wave: critical with regression test -> complete', c.r.complete === true)
  ok('contract-wave: default models all sonnet', c.models.length > 0 && c.models.every(([, m]) => m === 'sonnet'))
})()
;(async () => {
  // layered-wave: macro plan validation, level order, fail-closed stop.
  const R = '/tmp/x/repo'
  const ok = (name, cond) => { console.log((cond ? 'PASS ' : 'FAIL ') + name); if (!cond) process.exitCode = 1 }
  const G = { id: 'g1', statement: 's', criteria: [{ id: 'c1', text: 'leaf built' }, { id: 'c2', text: 'wired' }] }
  const leaf = (id, crate, deps = [], criteria = ['c1']) => ({ id, kind: 'leaf', crate, files: [`${crate}/Cargo.toml`, `${crate}/src/lib.rs`], depends_on: deps, spec: `build ${id}`, tests: [`test_${id}`], criteria })
  const wire = (id, files, deps, criteria = ['c2']) => ({ id, kind: 'wiring', files, depends_on: deps, spec: `wire ${id}`, tests: [], criteria })
  const plan = units => ({ decisions_needed: [], summary: 's', units })
  const levelsSeen = []
  const respond = done => async (l, a) => {
    if (l.startsWith('workflow:')) { levelsSeen.push(a.clusters.map(c => c.id).join('+')); return { complete: done(a), deltas: [] } }
    if (l.startsWith('goal:')) return { criteria: [{ id: 'c1', status: 'met', evidence: 'a/src/lib.rs:1' }, { id: 'c2', status: 'met', evidence: 'harw-runtime/src/assembly.rs:1' }], deltas: [] }
    return null
  }
  const good = plan([leaf('a', 'harw-a'), leaf('b', 'harw-b', ['a']), leaf('c', 'harw-c'), wire('w', ['harw-runtime/src/assembly.rs', 'Cargo.toml'], ['b', 'c'])])
  levelsSeen.length = 0
  let o = await run('layered-wave.js', { root: R, key: 'k', goal: G, plan: good }, respond(() => true), true)
  ok('layered: levels follow dependencies (a+c, then b, then wiring)', JSON.stringify(levelsSeen) === JSON.stringify(['a+c', 'b', 'w']))
  ok('layered: all levels + goal met -> complete, never achieved', o.r.complete === true && o.r.achieved === false && o.r.achieve === 'human-only')
  levelsSeen.length = 0
  o = await run('layered-wave.js', { root: R, key: 'k', goal: G, plan: good }, respond(a => !a.clusters.some(c => c.id === 'b')), true)
  ok('layered: failed leaf level stops before wiring', o.r.complete === false && o.r.stoppedAt === 'L2' && !levelsSeen.includes('w'))
  const bad = [
    ['file in two units', plan([leaf('a', 'harw-a'), { ...leaf('b', 'harw-b'), files: ['harw-b/src/lib.rs', 'harw-a/src/lib.rs'] }, wire('w', ['x/y.rs'], ['a'])])],
    ['leaf file outside its crate', plan([{ ...leaf('a', 'harw-a'), files: ['harw-a/src/lib.rs', 'harw-z/src/lib.rs'] }, wire('w', ['x/y.rs'], ['a'])])],
    ['crate split across leaf units', plan([leaf('a', 'harw-a'), { ...leaf('b', 'harw-a'), files: ['harw-a/src/other.rs'] }, wire('w', ['x/y.rs'], ['a'])])],
    ['leaf depends on wiring', plan([leaf('a', 'harw-a', ['w']), wire('w', ['x/y.rs'], [])])],
    ['cycle', plan([leaf('a', 'harw-a', ['b']), leaf('b', 'harw-b', ['a']), wire('w', ['x/y.rs'], ['a'])])],
    ['unknown dependency', plan([leaf('a', 'harw-a', ['nope']), wire('w', ['x/y.rs'], ['a'])])],
    ['criterion without unit', plan([leaf('a', 'harw-a'), wire('w', ['x/y.rs'], ['a'], ['c1'])])],
    ['absolute path outside root', plan([leaf('a', 'harw-a'), wire('w', ['/etc/passwd'], ['a'])])],
  ]
  for (const [name, p] of bad) {
    levelsSeen.length = 0
    const r = await run('layered-wave.js', { root: R, key: 'k', goal: G, plan: p }, respond(() => true), true)
    ok(`layered: plan rejected (${name}), nothing runs`, r.r && r.r.status === 'plan-invalid' && levelsSeen.length === 0)
  }
  levelsSeen.length = 0
  o = await run('layered-wave.js', { root: R, key: 'k', goal: G, plan: { ...good, decisions_needed: ['pick a default'] } }, respond(() => true), true)
  ok('layered: open owner decision stops before any level', o.r.status === 'needs-decision' && levelsSeen.length === 0)
  const passed = []
  await run('layered-wave.js', { root: R, key: 'k', goal: G, plan: good, decided: ['cap stays 16'] }, async (l, a) => { if (l.startsWith('workflow:')) passed.push(a.decided); return respond(() => true)(l, a) }, true)
  ok('layered: decided reaches every level', passed.length === 3 && passed.every(d => d.length === 1 && d[0] === 'cap stays 16'))
  let prompt = ''
  await run('contract-wave.js', { root: R, key: 'w', decided: ['KEK is bootstrapped'], clusters: [{ id: 'c1', files: ['a/src/x.rs'], findings: [{ file: 'a/src/x.rs', line: 1, severity: 'low', pattern: 'M1', title: 't', description: 'd', evidence: 'e', fix: 'f' }] }] }, async (l, p) => { if (l.startsWith('contract:')) prompt = p; return null })
  ok('contract-wave: decided listed as resolved in the contract prompt', prompt.includes('KEK is bootstrapped') && prompt.includes('never list them under decisions_needed again'))
})()
