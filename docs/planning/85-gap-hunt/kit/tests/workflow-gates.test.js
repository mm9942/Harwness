// Gate tests for the kit workflows, run with plain `node` (no dependencies):
//   node docs/planning/85-gap-hunt/kit/tests/workflow-gates.test.js
// Each workflow runs against a fake agent() that returns canned results, so the
// tests check the deterministic control flow only: root validation, the ripple
// gate, the contract file-set check and when repairs may run.
const fs = require('fs')
const K = require('path').join(__dirname, '..', 'workflows') + '/'
function load(f) { return fs.readFileSync(K + f, 'utf8').replace(/^export const meta/m, 'const meta') }
async function run(f, args, respond) {
  const calls = []
  const agent = async (prompt, opts) => { calls.push(opts.label); return respond(opts.label, prompt) }
  const parallel = async ts => Promise.all(ts.map(t => t().catch(() => null)))
  const pipeline = async (items, ...stages) => Promise.all(items.map(async (it, i) => { let v = it; for (const s of stages) { try { v = await s(v, it, i) } catch (e) { return null } } return v }))
  const fn = new Function('args', 'agent', 'parallel', 'pipeline', 'phase', 'log', 'return (async()=>{' + load(f) + '})()')
  try { const r = await fn(args, agent, parallel, pipeline, () => {}, () => {}); return { r, calls } } catch (e) { return { err: e.message, calls } }
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
