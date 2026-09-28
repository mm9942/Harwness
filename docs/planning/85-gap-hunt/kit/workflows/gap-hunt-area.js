export const meta = {
  name: 'gap-hunt-area',
  description: 'Read-only gap hunt for one crate group: pattern-lens Opus finders + completeness critic, 2-lens verify with tie-breaker; no edits, no builds',
  whenToUse: 'One run per disjoint crate group (parallel cut: the concurrency cap is per workflow). Consolidate the returned findings, then run gap-fix per disjoint file set.',
  phases: [
    { title: 'Find', detail: 'Opus finders, one per pattern lens (fail-open, correctness/concurrency, rules/drift)' },
    { title: 'Critic', detail: 'Opus completeness critic: what did the finders miss?' },
    { title: 'Verify', detail: 'tiered by catalog P9/P10: M3 intent-only, critical/high intent+scope(+exploit for M1), rest intent+reproduce; tie-breaker on split (sonnet)' },
  ],
}

// Args:
//   key      short area id (labels, logs)
//   area     human description of the area
//   crates   list of crates/paths to sweep
//   exclude  paths hunted elsewhere in parallel (do not report)
//   catalog  pattern catalog path (default docs/planning/85-gap-hunt/patterns.md)
//   rules    extra binding code rules (default: the workspace rules below)
//   verify   'tiered' (default, catalog P9/P10) or 'classic' (reproduce+intent for every finding)
const A = args || {}
const CATALOG = A.catalog || 'docs/planning/85-gap-hunt/patterns.md'
const RULES = A.rules || 'no let-chains (`if let … && …`, MSRV 1.85), forbid(unsafe), no unwrap/expect/panic! in library code OR tests/doctests (tests return TestResult), no third-party types in public APIs, hand-written error types, match the file\'s comment language, no book titles/authors/quotes anywhere'
const BUILD_RULE = 'Subagents and parallel agents must **never** run `cargo` or `rustc` in any form: no `check`, `build`, `test`, `nextest`, `clippy`, `fmt`, `run`, `doc`, `deny`, and no `make` target that calls them. They only read and edit code. At the end they report which tests they added and which commands the central build must run.'

const CONTEXT = `Repository: the current working directory (Rust workspace). Read the current working tree; another process may be editing some files concurrently, so re-read before concluding.
Your area: ${A.area}. Crates/paths: ${(A.crates || []).join(', ')}.
Out of scope (hunted elsewhere right now, do not report): ${(A.exclude || []).join(', ') || 'none'}.
Binding code rules: ${RULES}. Design decisions: docs/planning/70-decisions; guides: docs/guides; ledger: docs/planning/90-migration-ledger.
Pattern catalog: ${CATALOG} (read it first). Tag every finding with a catalog id (M1…, P5, P8, …) or NEW:<short-name>.
READ-ONLY: do not edit, create or delete any file. ${BUILD_RULE}`

const FINDINGS = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          file: { type: 'string', description: 'repo-relative path' },
          line: { type: 'integer' },
          severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
          category: { type: 'string' },
          pattern: { type: 'string', description: 'catalog id or NEW:<name>' },
          title: { type: 'string' },
          description: { type: 'string' },
          evidence: { type: 'string' },
          fix: { type: 'string' },
          single_file: { type: 'boolean' },
          other_files: { type: 'array', items: { type: 'string' } },
          test_idea: { type: 'string' },
        },
        required: ['file', 'line', 'severity', 'category', 'pattern', 'title', 'description', 'evidence', 'fix', 'single_file'],
      },
    },
  },
  required: ['findings'],
}
const VERDICT = {
  type: 'object',
  properties: { real: { type: 'boolean' }, fixable_in_one_file: { type: 'boolean' }, reason: { type: 'string' } },
  required: ['real', 'fixable_in_one_file', 'reason'],
}

const LENSES_FIND = A.lenses || [
  { key: 'failopen', text: 'Catalog M1 and security: checks after/inside failure branches instead of before the success path, lenient fallbacks and default-allow, missing authorization/tenant filters, untrusted config layers that can widen rights, unvalidated input across a trust boundary, permissions left to umask, TOCTOU, secrets in logs/errors, enforcement that can be skipped; M7 temp files.' },
  { key: 'correctness', text: 'Correctness and robustness: logic errors, edge cases, overflow, M5 unbounded reads/buffers/awaits without timeout, M6 detached tasks, cancellation not propagated, locks across await, resource leaks, errors logged where they must stop the flow, restart/recovery gaps, M2 limit/accounting drift across layers, M8 multi-process file coordination.' },
  { key: 'rules-drift', text: 'Rules and drift: P5 unwrap/expect/panic! in tests and doctests, P8 let-chains, public third-party types, unused dependencies, dead feature flags, book/author references; M3 docs/comments/guides that contradict the code; todo!/unimplemented!/stubs that silently succeed; pub items without a production caller.' },
]
const LENSES_VERIFY = [
  { key: 'reproduce', text: 'Correctness / does-it-reproduce: read the cited code yourself on the current working tree. Default to real=false if the evidence does not hold.' },
  { key: 'intent', text: 'Intent: is this deliberate/documented (decisions, guides, comments) or handled elsewhere (grep callers and other layers)? If so real=false. For doc drift (M3) confirm cheaply by grepping the cited text and the code it describes.' },
  { key: 'scope', text: 'Fix scope and risk: genuine defect worth fixing now (not style noise), and fixable by editing ONLY the primary file without breaking callers elsewhere? fixable_in_one_file=false if other files must change.' },
  { key: 'exploit', text: 'Exploitability (adversarial): name a concrete input, caller or config that an untrusted party controls and that reaches the flaw. If no untrusted party can reach it, real=false and say "defence in depth" in the reason.' },
]
const lens = k => LENSES_VERIFY.find(l => l.key === k)

// Which lenses judge a finding (catalog P9/P10): intent is the only lens that
// really discriminates, reproduce almost never refutes precise Opus finders,
// M3 survives 98 % and is cheap to confirm by grep, critical/high survive
// 100 % and need a scope/risk check rather than an existence proof. The
// second list is the tie-breaker when the first votes split.
const plan = f => {
  if (A.verify === 'classic') return [['reproduce', 'intent'], 'scope']
  if (String(f.pattern || '').startsWith('M3')) return [['intent'], 'reproduce']
  if (f.severity === 'critical' || f.severity === 'high') {
    return [String(f.pattern || '').startsWith('M1') ? ['intent', 'scope', 'exploit'] : ['intent', 'scope'], 'reproduce']
  }
  return [['intent', 'reproduce'], 'scope']
}

const norm = p => String(p || '').replace(/^\/.*?\/(?=[^/]+\/src\/|docs\/|dod\/|deploy\/|xtask\/|Cargo\.toml)/, '').replace(/^\.\//, '')
const seen = new Set()
const titles = []
const confirmed = []
const rejected = []
const unverified = []

const verifyOne = (f, l) => agent(`${CONTEXT}

You are an adversarial verifier. Try to REFUTE this finding. Lens: ${l.text}

Finding (${f.severity}, ${f.pattern}, ${f.category}): ${f.title}
Primary file: ${f.file}:${f.line}
Description: ${f.description}
Evidence: ${f.evidence}
Proposed fix: ${f.fix}
single_file: ${f.single_file}; other files: ${(f.other_files || []).join(', ') || 'none'}`,
  { label: `verify:${l.key}:${f.file.split('/').pop()}:${f.line}`, phase: 'Verify', schema: VERDICT, model: 'sonnet', agentType: 'focused-explorer' })

const judge = async f => {
  const [firstKeys, tieKey] = plan(f)
  const run = k => verifyOne(f, lens(k)).then(v => (v ? { ...v, lens: k } : null))
  const first = (await parallel(firstKeys.map(k => () => run(k)))).filter(Boolean)
  let votes = first
  const yes = key => first.filter(v => v[key]).length
  const tied = key => 2 * yes(key) === first.length
  // Missing votes, an exact tie on "real", or a real finding whose one-file
  // verdicts tie: ask the tie-breaker lens. An odd panel never ties.
  if (first.length < firstKeys.length || tied('real') || (2 * yes('real') > first.length && tied('fixable_in_one_file'))) {
    const tie = await run(tieKey)
    votes = [...first, tie].filter(Boolean)
  }
  // Fixed quorum: as many answered lenses as were planned (the tie-breaker may
  // stand in for a lens that died). Fewer answers never decide a finding in
  // either direction; it stays unverified and can be retried.
  if (votes.length < firstKeys.length) {
    unverified.push({ ...f, votes: votes.map(v => ({ lens: v.lens, real: v.real })), reason: `${votes.length} of ${firstKeys.length} planned verdicts` })
    return
  }
  const need = Math.floor(votes.length / 2) + 1
  const real = votes.filter(v => v.real).length >= need
  const oneFile = f.single_file && votes.filter(v => v.fixable_in_one_file).length >= need
  const tally = votes.map(v => ({ lens: v.lens, real: v.real }))
  if (real) confirmed.push({ ...f, oneFile, votes: tally, reasons: votes.map(v => v.reason) })
  else rejected.push({ file: f.file, line: f.line, title: f.title, votes: tally, reasons: votes.map(v => v.reason) })
}

// Dedup by file + 20-line bucket, ignoring category (several lenses often
// report the same spot under different categories).
const freshOf = r => {
  const out = []
  for (const f of (r && r.findings) || []) {
    f.file = norm(f.file)
    const k = `${f.file}:${Math.round((f.line || 0) / 20)}`
    if (seen.has(k)) continue
    seen.add(k)
    titles.push(`${f.file}:${f.line} ${f.title}`)
    out.push(f)
  }
  return out
}

phase('Find')
await pipeline(LENSES_FIND,
  l => agent(`${CONTEXT}

You are a read-only gap finder. Lens: ${l.key}. ${l.text}
Sweep the area systematically (list the crates' source files, grep for risky constructs, then deep-read the hot spots). Report only REAL, evidenced defects with exact file:line, quotes, and a concrete fix. Mark single_file=false and list other_files if the fix needs more than one file.`,
    { label: `find:${A.key}:${l.key}`, phase: 'Find', schema: FINDINGS, model: 'opus', agentType: 'focused-explorer' }),
  r => parallel(freshOf(r).map(f => () => judge(f))),
)

phase('Critic')
const critic = await agent(`${CONTEXT}

You are the completeness critic for this area. The finders already reported (do not repeat):
- ${titles.join('\n- ') || '(nothing)'}
Ask: which crates/files/modules were NOT examined, which risk classes were not checked (concurrency, error paths, input limits, auth, persistence/recovery, docs drift), and look there. Report only real, evidenced new defects.`,
  { label: `critic:${A.key}`, phase: 'Critic', schema: FINDINGS, model: 'opus', agentType: 'focused-explorer' })
await parallel(freshOf(critic).map(f => () => judge(f)))

log(`${A.key}: ${confirmed.length} confirmed (${confirmed.filter(c => c.oneFile).length} single-file), ${rejected.length} rejected, ${unverified.length} unverified`)
return { area: A.key, confirmed, rejected, unverified }
